//! The transport layer (RFC 4253): the version lines, binary packets and their
//! sequence numbers, the key exchange and every re-exchange the client asks
//! for, and the keys.
//!
//! **Strict key exchange, always** (OpenSSH `PROTOCOL` §1.10, CVE-2023-48795).
//! A client that does not offer it is refused. In the first exchange the
//! client's KEXINIT is its first packet and every message but the next one of
//! the exchange is refused — `IGNORE` and `DEBUG` too, which are what a
//! prefix-truncation attack injects; and at every `NEWKEYS` the sequence number
//! of that direction starts again at zero, so a packet dropped before it
//! cannot be made up for by one injected.
//!
//! **A length before its tag is a claim.** `chacha20-poly1305@openssh.com`
//! encrypts the packet length under its own key: the decrypted length is held
//! to the cap and the block size and used for nothing but knowing how many
//! bytes to wait for, until the tag over the whole packet verifies.
//!
//! **During a re-exchange the server sends nothing but the exchange**: a
//! payload the session layer sends between the server's KEXINIT and its
//! NEWKEYS is held, and sent under the new keys (RFC 4253 §7.1).

use ring::aead::chacha20_poly1305_openssh::{OpeningKey, SealingKey, KEY_LEN, TAG_LEN};
use ring::agreement;
use ring::rand::SecureRandom;

use crate::hostkey::HostKey;
use crate::kex::{self, Exchange, Method, Negotiated};
use crate::msg;
use crate::wire::{put_mpint, put_string, put_u32, Reader, Refusal};

/// The server's identification (RFC 4253 §4.2), without its CR LF.
const VERSION: &[u8] = b"SSH-2.0-ToyOS";

/// The longest client identification line, CR LF included (RFC 4253 §4.2).
const VERSION_CAP: usize = 255;

/// The longest `packet_length` before authentication: the size every
/// implementation must take (RFC 4253 §6.1), so an unauthenticated peer makes
/// the server hold no more.
const PREAUTH_PACKET_CAP: usize = 35_000;

/// The longest `packet_length` once authenticated: OpenSSH's own maximum.
pub(crate) const PACKET_CAP: usize = 256 * 1024;

/// The cipher's block, which every packet's length is a multiple of.
const BLOCK: usize = 8;
const MIN_PADDING: usize = 4;

/// What [`Transport::next`] hands up.
pub(crate) enum Incoming {
    /// A payload for the authentication or connection layer, and the session
    /// it belongs to.
    Payload(Vec<u8>, [u8; 32]),
    /// The client said goodbye (`SSH_MSG_DISCONNECT`).
    Disconnect,
}

enum Kex {
    /// The server's KEXINIT is sent and the client's awaited: the session's
    /// start, so it is the client's first packet.
    Started,
    /// Both KEXINITs are exchanged and the client's `KEX_ECDH_INIT` is awaited.
    Negotiated { negotiated: Negotiated, client_kexinit: Vec<u8> },
    /// The server's NEWKEYS is sent; the client's is awaited, after which its
    /// packets open with `opening`.
    NewKeys { opening: Box<OpeningKey> },
    /// No exchange is in flight.
    Idle,
}

pub(crate) struct Transport<R> {
    rng: R,
    host: HostKey,
    /// The client's identification line, once read.
    client_version: Option<Vec<u8>>,
    /// The client's bytes; those before `read` are read, and dropped only at
    /// the next input, so one input of many packets moves its bytes once.
    inbound: Vec<u8>,
    read: usize,
    recv_seq: u32,
    send_seq: u32,
    opening: Option<Box<OpeningKey>>,
    sealing: Option<Box<SealingKey>>,
    out: Vec<u8>,
    /// The server's KEXINIT payload of the exchange in flight, or of the last.
    server_kexinit: Vec<u8>,
    kex: Kex,
    /// `H` of the first exchange (RFC 4253 §7.2), once there was one.
    session_id: Option<[u8; 32]>,
    /// Payloads sent during a re-exchange, held until the server's NEWKEYS.
    held: Vec<Vec<u8>>,
    packet_cap: usize,
}

impl<R: SecureRandom> Transport<R> {
    pub(crate) fn new(host: HostKey, rng: R) -> Self {
        let mut transport = Self {
            rng,
            host,
            client_version: None,
            inbound: Vec::new(),
            read: 0,
            recv_seq: 0,
            send_seq: 0,
            opening: None,
            sealing: None,
            out: [VERSION, b"\r\n"].concat(),
            server_kexinit: Vec::new(),
            kex: Kex::Started,
            session_id: None,
            held: Vec::new(),
            packet_cap: PREAUTH_PACKET_CAP,
        };
        transport.send_kexinit().expect("the first packet has a sequence number");
        transport
    }

    pub(crate) fn feed(&mut self, bytes: &[u8]) {
        self.inbound.drain(..self.read);
        self.read = 0;
        self.inbound.extend_from_slice(bytes);
    }

    pub(crate) fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.out)
    }

    /// Lift the packet cap to [`PACKET_CAP`], once the client is authenticated.
    pub(crate) fn authenticated(&mut self) {
        self.packet_cap = PACKET_CAP;
    }

    /// Send a payload of the authentication or connection layer, or hold it
    /// while a re-exchange is in flight.
    pub(crate) fn send(&mut self, payload: Vec<u8>) -> Result<(), Refusal> {
        match self.kex {
            Kex::Started | Kex::Negotiated { .. } => {
                self.held.push(payload);
                Ok(())
            }
            Kex::NewKeys { .. } | Kex::Idle => self.seal(&payload),
        }
    }

    /// Tell the client why its session ends: nothing after a tag that did not
    /// verify, and nothing before the client has said it speaks SSH.
    pub(crate) fn refuse(&mut self, refusal: &Refusal) {
        let reason = match refusal {
            Refusal::Integrity | Refusal::SealedLength { .. } | Refusal::SequenceExhausted | Refusal::Ended => return,
            Refusal::TooManyAttempts => msg::DISCONNECT_NO_MORE_AUTH_METHODS_AVAILABLE,
            Refusal::NoCommonAlgorithm(_) | Refusal::NotStrict | Refusal::KeyAgreement => msg::DISCONNECT_KEY_EXCHANGE_FAILED,
            Refusal::Truncated(_)
            | Refusal::TooLong { .. }
            | Refusal::Trailing(_)
            | Refusal::NotUtf8(_)
            | Refusal::Malformed(_)
            | Refusal::Unexpected { .. } => msg::DISCONNECT_PROTOCOL_ERROR,
        };
        if self.client_version.is_none() {
            return;
        }
        let mut payload = vec![msg::DISCONNECT];
        put_u32(&mut payload, reason);
        put_string(&mut payload, refusal.to_string().as_bytes());
        put_string(&mut payload, b"");
        // The session's last packet: one sealed at the last sequence number
        // reuses no nonce, and nothing follows it.
        let _ = self.seal(&payload);
    }

    /// The next payload for the layers above, with every transport message
    /// before it handled.
    pub(crate) fn next(&mut self) -> Result<Option<Incoming>, Refusal> {
        if self.client_version.is_none() {
            match self.read_version()? {
                Some(line) => self.client_version = Some(line),
                None => return Ok(None),
            }
        }
        loop {
            let Some(payload) = self.read_packet()? else { return Ok(None) };
            self.recv_seq = self.recv_seq.checked_add(1).ok_or(Refusal::SequenceExhausted)?;
            let &kind = payload.first().ok_or(Refusal::Malformed("an empty payload"))?;
            // The first exchange lasts until the client's first NEWKEYS, after
            // the session identifier is fixed.
            let first = self.opening.is_none();
            let phase = match (&self.kex, first) {
                (Kex::Idle, _) => {
                    match kind {
                        msg::KEXINIT => self.on_kexinit(payload)?,
                        msg::DISCONNECT => return Ok(Some(Incoming::Disconnect)),
                        msg::IGNORE | msg::DEBUG | msg::UNIMPLEMENTED => {}
                        msg::SERVICE_REQUEST | msg::USERAUTH_FIRST.. => {
                            let id = self.session_id.expect("the transport is idle only once the first exchange fixed the session");
                            return Ok(Some(Incoming::Payload(payload, id)));
                        }
                        _ => return Err(Refusal::Unexpected { phase: "the transport", message: kind }),
                    }
                    continue;
                }
                (_, true) => "the first key exchange",
                (_, false) => "a key re-exchange",
            };
            match (std::mem::replace(&mut self.kex, Kex::Idle), kind) {
                (Kex::Started, msg::KEXINIT) => self.on_kexinit(payload)?,
                (Kex::Negotiated { negotiated, client_kexinit }, msg::KEX_ECDH_INIT) => {
                    self.on_ecdh_init(&payload, &negotiated, &client_kexinit)?;
                }
                (Kex::NewKeys { opening }, msg::NEWKEYS) => {
                    Reader::new(&payload[1..]).end("NEWKEYS")?;
                    self.opening = Some(opening);
                    self.recv_seq = 0;
                }
                (_, msg::DISCONNECT) => return Ok(Some(Incoming::Disconnect)),
                (state, msg::IGNORE | msg::DEBUG | msg::UNIMPLEMENTED) if !first => self.kex = state,
                (_, message) => return Err(Refusal::Unexpected { phase, message }),
            }
        }
    }

    /// The client's identification line, without its CR LF, once all of it
    /// has come.
    fn read_version(&mut self) -> Result<Option<Vec<u8>>, Refusal> {
        let unread = &self.inbound[self.read..];
        let Some(end) = unread.windows(2).position(|w| w == b"\r\n") else {
            if unread.len() >= VERSION_CAP {
                return Err(Refusal::TooLong { field: "identification line", len: unread.len(), cap: VERSION_CAP });
            }
            return Ok(None);
        };
        if end + 2 > VERSION_CAP {
            return Err(Refusal::TooLong { field: "identification line", len: end + 2, cap: VERSION_CAP });
        }
        let line = unread[..end].to_vec();
        self.read += end + 2;
        if !line.starts_with(b"SSH-2.0-") {
            return Err(Refusal::Malformed("an identification line that is not SSH-2.0"));
        }
        if !line.iter().all(|&b| (0x20..0x7f).contains(&b)) {
            return Err(Refusal::Malformed("an identification line that is not printable US-ASCII"));
        }
        Ok(Some(line))
    }

    /// The next packet's payload, once all of it has come and its tag has
    /// verified.
    fn read_packet(&mut self) -> Result<Option<Vec<u8>>, Refusal> {
        let unread = &self.inbound[self.read..];
        let Some(&head) = unread.first_chunk::<4>() else { return Ok(None) };
        let length = match &self.opening {
            None => head,
            Some(key) => key.decrypt_packet_length(self.recv_seq, head),
        };
        let len = u32::from_be_bytes(length) as usize;
        let cap = self.packet_cap;
        let (aligned, tag) = match self.opening {
            None => ((4 + len) % BLOCK, 0),
            Some(_) => (len % BLOCK, TAG_LEN),
        };
        let framed = len <= cap && aligned == 0 && len > MIN_PADDING;
        match (framed, &self.opening) {
            (true, _) => {}
            (false, Some(_)) => return Err(Refusal::SealedLength { len, cap }),
            (false, None) if len > cap => return Err(Refusal::TooLong { field: "packet_length", len, cap }),
            (false, None) => return Err(Refusal::Malformed("a packet_length that is short or not a multiple of the block")),
        }
        let total = 4 + len + tag;
        let Some(packet) = unread.get(..total) else { return Ok(None) };
        let mut packet = packet.to_vec();
        self.read += total;
        let body: &[u8] = match &self.opening {
            None => packet.get(4..).unwrap_or_default(),
            Some(key) => {
                let (sealed, tag) = packet.split_at_mut(4 + len);
                let tag: &[u8; TAG_LEN] = (&*tag).try_into().map_err(|_| Refusal::Integrity)?;
                key.open_in_place(self.recv_seq, sealed, tag).map_err(|_| Refusal::Integrity)?
            }
        };
        let (&padding, rest) = body.split_first().ok_or(Refusal::Truncated("padding_length"))?;
        let padding = usize::from(padding);
        if padding < MIN_PADDING {
            return Err(Refusal::Malformed("padding under four bytes"));
        }
        let payload = rest.len().checked_sub(padding).ok_or(Refusal::Malformed("padding longer than its packet"))?;
        Ok(Some(rest[..payload].to_vec()))
    }

    /// Frame, pad and seal `payload` into the output.
    fn seal(&mut self, payload: &[u8]) -> Result<(), Refusal> {
        let unaligned = if self.sealing.is_some() { 1 + payload.len() } else { 4 + 1 + payload.len() };
        let mut padding = BLOCK - unaligned % BLOCK;
        if padding < MIN_PADDING {
            padding += BLOCK;
        }
        let len = 1 + payload.len() + padding;
        let mut packet = Vec::with_capacity(4 + len + TAG_LEN);
        put_u32(&mut packet, u32::try_from(len).expect("a packet the server builds fits a u32"));
        packet.push(u8::try_from(padding).expect("padding is under two blocks"));
        packet.extend_from_slice(payload);
        let start = packet.len();
        packet.resize(start + padding, 0);
        self.rng.fill(&mut packet[start..]).expect("the system's randomness");
        if let Some(key) = &self.sealing {
            let mut tag = [0; TAG_LEN];
            key.seal_in_place(self.send_seq, &mut packet, &mut tag);
            packet.extend_from_slice(&tag);
        }
        self.out.extend_from_slice(&packet);
        self.send_seq = self.send_seq.checked_add(1).ok_or(Refusal::SequenceExhausted)?;
        Ok(())
    }

    fn send_kexinit(&mut self) -> Result<(), Refusal> {
        let mut cookie = [0; 16];
        self.rng.fill(&mut cookie).expect("the system's randomness");
        self.server_kexinit = kex::kexinit(cookie);
        let payload = self.server_kexinit.clone();
        self.seal(&payload)
    }

    fn on_kexinit(&mut self, payload: Vec<u8>) -> Result<(), Refusal> {
        let negotiated = kex::negotiate(&payload, self.session_id.is_none())?;
        if self.session_id.is_some() {
            self.send_kexinit()?;
        }
        self.kex = Kex::Negotiated { negotiated, client_kexinit: payload };
        Ok(())
    }

    fn on_ecdh_init(&mut self, payload: &[u8], negotiated: &Negotiated, client_kexinit: &[u8]) -> Result<(), Refusal> {
        let mut r = Reader::new(payload);
        r.byte("KEX_ECDH_INIT")?;
        match negotiated.method {
            Method::Curve25519Sha256 => {}
        }
        let client_public: [u8; 32] = r.fixed("Q_C")?;
        r.end("KEX_ECDH_INIT")?;
        let private = agreement::EphemeralPrivateKey::generate(&agreement::X25519, &self.rng).expect("the system's randomness");
        let server_public = private.compute_public_key().expect("an X25519 public key");
        let peer = agreement::UnparsedPublicKey::new(&agreement::X25519, client_public);
        let mut secret = Vec::new();
        agreement::agree_ephemeral(private, &peer, |k| put_mpint(&mut secret, k)).map_err(|_| Refusal::KeyAgreement)?;
        let host_key = self.host.blob();
        let hash = Exchange {
            client_version: self.client_version.as_deref().unwrap_or_default(),
            server_version: VERSION,
            client_kexinit,
            server_kexinit: &self.server_kexinit,
            host_key: &host_key,
            client_public: &client_public,
            server_public: server_public.as_ref(),
            secret: &secret,
        }
        .hash();
        let session_id = *self.session_id.get_or_insert(hash);
        let mut reply = vec![msg::KEX_ECDH_REPLY];
        put_string(&mut reply, &host_key);
        put_string(&mut reply, server_public.as_ref());
        put_string(&mut reply, &self.host.sign(&hash));
        self.seal(&reply)?;
        self.seal(&[msg::NEWKEYS])?;
        let key = |letter| -> [u8; KEY_LEN] { kex::derive(&secret, &hash, letter, &session_id) };
        self.sealing = Some(Box::new(SealingKey::new(&key(b'D'))));
        self.send_seq = 0;
        if negotiated.ext_info {
            let mut info = vec![msg::EXT_INFO];
            put_u32(&mut info, 1);
            put_string(&mut info, b"server-sig-algs");
            put_string(&mut info, crate::hostkey::ALGORITHM.as_bytes());
            self.seal(&info)?;
        }
        for held in std::mem::take(&mut self.held) {
            self.seal(&held)?;
        }
        self.kex = Kex::NewKeys { opening: Box::new(OpeningKey::new(&key(b'C'))) };
        Ok(())
    }
}
