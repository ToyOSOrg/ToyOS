//! A client written beside the server, from the RFCs and on `toyos-crypto`'s
//! primitives directly: its own exchange hash, key derivation and packet
//! layer, so a test can say anything to the server, in plaintext or sealed, in
//! any order.

use std::sync::OnceLock;

use toyos_crypto::ed25519::{self, SigningKey};
use toyos_crypto::hash::Sha256;
use toyos_crypto::ssh::{OpeningKey, SealingKey};
use toyos_crypto::{mlkem, x25519};

use toyos_ssh::{Event, Random, Refusal, Server};

use super::driver::Keys;

pub const STRICT: &str = "kex-strict-c-v00@openssh.com";

pub const HYBRID: &str = "mlkem768x25519-sha256";
pub const CURVE25519: &str = "curve25519-sha256";

/// The Poly1305 tag after every sealed packet.
pub const TAG_LEN: usize = 16;

pub fn string(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
}

pub fn mpint(magnitude: &[u8]) -> Vec<u8> {
    let digits: Vec<u8> = magnitude.iter().copied().skip_while(|&b| b == 0).collect();
    let mut body = Vec::new();
    if digits.first().is_some_and(|&b| b & 0x80 != 0) {
        body.push(0);
    }
    body.extend_from_slice(&digits);
    let mut out = Vec::new();
    string(&mut out, &body);
    out
}

pub fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    let mut c = Sha256::new();
    for p in parts {
        c.update(p).unwrap();
    }
    c.finalize()
}

/// `K` as RFC 8731 §3 encodes the X25519 secret: an `mpint`.
pub fn curve25519_k(shared: &[u8; 32]) -> Vec<u8> {
    mpint(shared)
}

/// `K` as RFC 10042 §2.4 and §2.5 make it: `SHA-256(K_PQ || K_CL)`, a `string`.
pub fn hybrid_k(k_pq: &[u8; 32], k_cl: &[u8; 32]) -> Vec<u8> {
    let mut out = Vec::new();
    string(&mut out, &sha256(&[k_pq, k_cl]));
    out
}

/// RFC 8731 §3's and RFC 10042 §2.5's `H`; `k` is encoded.
#[allow(clippy::too_many_arguments)]
pub fn exchange_hash(v_c: &[u8], v_s: &[u8], i_c: &[u8], i_s: &[u8], k_s: &[u8], q_c: &[u8], q_s: &[u8], k: &[u8]) -> [u8; 32] {
    let mut data = Vec::new();
    for field in [v_c, v_s, i_c, i_s, k_s, q_c, q_s] {
        string(&mut data, field);
    }
    data.extend_from_slice(k);
    sha256(&[&data])
}

/// RFC 4253 §7.2, 64 bytes; `k` is encoded.
pub fn derive(k: &[u8], h: &[u8; 32], letter: u8, session_id: &[u8; 32]) -> [u8; 64] {
    let k1 = sha256(&[k, h, &[letter], session_id]);
    let k2 = sha256(&[k, h, &k1]);
    [k1, k2].concat().try_into().unwrap()
}

/// One field reader for what the server sends; a test's own, so it panics.
pub struct Fields<'a>(pub &'a [u8]);

impl<'a> Fields<'a> {
    pub fn byte(&mut self) -> u8 {
        let b = self.0[0];
        self.0 = &self.0[1..];
        b
    }
    pub fn u32(&mut self) -> u32 {
        let v = u32::from_be_bytes(self.0[..4].try_into().unwrap());
        self.0 = &self.0[4..];
        v
    }
    pub fn string(&mut self) -> &'a [u8] {
        let len = self.u32() as usize;
        let s = &self.0[..len];
        self.0 = &self.0[len..];
        s
    }
}

/// A plaintext packet around `payload`, padded as RFC 4253 §6 asks.
pub fn plain_packet(payload: &[u8]) -> Vec<u8> {
    let mut padding = 8 - (5 + payload.len()) % 8;
    if padding < 4 {
        padding += 8;
    }
    let mut out = ((1 + payload.len() + padding) as u32).to_be_bytes().to_vec();
    out.push(padding as u8);
    out.extend_from_slice(payload);
    out.extend(std::iter::repeat_n(0, padding));
    out
}

/// A sealed packet around `payload` (`PROTOCOL.chacha20poly1305`), at the
/// key's next sequence number.
pub fn sealed_packet(key: &mut SealingKey, payload: &[u8]) -> Vec<u8> {
    let mut padding = 8 - (1 + payload.len()) % 8;
    if padding < 4 {
        padding += 8;
    }
    let mut out = ((1 + payload.len() + padding) as u32).to_be_bytes().to_vec();
    out.push(padding as u8);
    out.extend_from_slice(payload);
    out.extend(std::iter::repeat_n(0, padding));
    let (header, body) = out.split_first_chunk_mut::<4>().unwrap();
    let (_, tag) = key.seal(header, body).unwrap();
    out.extend_from_slice(&tag);
    out
}

/// The next packet's payload in `bytes`, and how many bytes it took.
pub fn open_packet(key: Option<&OpeningKey>, seq: u32, bytes: &[u8]) -> Option<(Vec<u8>, usize)> {
    let head: [u8; 4] = bytes.get(..4)?.try_into().unwrap();
    let len = match key {
        Some(key) => key.packet_length(seq, head),
        None => u32::from_be_bytes(head),
    } as usize;
    let total = 4 + len + if key.is_some() { TAG_LEN } else { 0 };
    let packet = bytes.get(..total)?;
    let mut sealed = packet[..4 + len].to_vec();
    let (header, body) = sealed.split_first_chunk_mut::<4>().unwrap();
    if let Some(key) = key {
        key.open(seq, header, body, &packet[4 + len..]).expect("the server's packet opens");
    }
    let plain = body.to_vec();
    let padding = plain[0] as usize;
    Some((plain[1..plain.len() - padding].to_vec(), total))
}

/// A client's KEXINIT naming `kex` and the one set the server offers.
pub fn kexinit(kex: &str) -> Vec<u8> {
    let mut out = vec![20];
    out.extend_from_slice(&[0x11; 16]);
    for list in [kex, "ssh-ed25519", "chacha20-poly1305@openssh.com", "chacha20-poly1305@openssh.com", "", "", "none", "none", "", ""] {
        string(&mut out, list.as_bytes());
    }
    out.push(0);
    out.extend_from_slice(&[0; 4]);
    out
}

pub const VERSION: &[u8] = b"SSH-2.0-TestClient";

/// A client the test drives step by step, its server under it.
pub struct Client<R> {
    pub server: Server<Keys, R>,
    pub events: Vec<Event>,
    sealing: Option<SealingKey>,
    opening: Option<OpeningKey>,
    in_seq: u32,
    pending: Vec<u8>,
    server_version: Vec<u8>,
    pub session_id: Option<[u8; 32]>,
}

/// A key exchange the client has started.
pub struct Exchange {
    i_c: Vec<u8>,
    /// The client's ML-KEM-768 key when the server picks the hybrid: the first
    /// of the client's methods it runs.
    kem: Option<mlkem::DecapsulationKey>,
    private: x25519::SecretKey,
}

/// The client's ephemeral X25519 key, the same in every exchange.
const EPHEMERAL: [u8; 32] = [0x3c; 32];

/// The seeds of the client's ephemeral ML-KEM-768 key, the same in every
/// exchange.
struct KemSeed;

impl Random for KemSeed {
    fn fill(&mut self, destination: &mut [u8]) -> Result<(), toyos_crypto::Error> {
        destination.fill(0x5e);
        Ok(())
    }
}

impl<R: Random> Client<R> {
    /// A client that has sent its identification line and read the server's.
    pub fn new(server: Server<Keys, R>) -> Result<Self, Refusal> {
        let mut c = Self {
            server,
            events: Vec::new(),
            sealing: None,
            opening: None,
            in_seq: 0,
            pending: Vec::new(),
            server_version: Vec::new(),
            session_id: None,
        };
        c.pending.extend(c.server.output());
        let end = c.pending.windows(2).position(|w| w == b"\r\n").unwrap();
        c.server_version = c.pending.drain(..end + 2).take(end).collect();
        c.raw(&[VERSION, b"\r\n"].concat())?;
        Ok(c)
    }

    /// Hand the server raw bytes.
    pub fn raw(&mut self, bytes: &[u8]) -> Result<(), Refusal> {
        let result = self.server.input(bytes);
        while let Some(event) = self.server.poll() {
            self.events.push(event);
        }
        result
    }

    /// Send `payload` in a packet, sealed once keys are in place.
    pub fn send(&mut self, payload: &[u8]) -> Result<(), Refusal> {
        let packet = self.packet(payload);
        self.raw(&packet)
    }

    /// The packet `send` would send, its sequence number spent.
    pub fn packet(&mut self, payload: &[u8]) -> Vec<u8> {
        match &mut self.sealing {
            Some(key) => sealed_packet(key, payload),
            None => plain_packet(payload),
        }
    }

    /// The server's next payload, if all of it has come.
    pub fn next(&mut self) -> Option<Vec<u8>> {
        self.pending.extend(self.server.output());
        let (payload, used) = open_packet(self.opening.as_ref(), self.in_seq, &self.pending)?;
        self.pending.drain(..used);
        self.in_seq = self.in_seq.wrapping_add(1);
        Some(payload)
    }

    /// Every payload the server has sent since the last call.
    pub fn recv(&mut self) -> Vec<Vec<u8>> {
        std::iter::from_fn(|| self.next()).collect()
    }

    /// Send a KEXINIT naming `kex`.
    pub fn start_exchange(&mut self, kex: &str) -> Result<Exchange, Refusal> {
        let i_c = kexinit(kex);
        self.send(&i_c)?;
        let hybrid = kex.split(',').find(|name| [HYBRID, CURVE25519].contains(name)) == Some(HYBRID);
        let kem = hybrid.then(|| mlkem::DecapsulationKey::generate(&mut KemSeed).unwrap());
        Ok(Exchange { i_c, kem, private: x25519::SecretKey::from_bytes(EPHEMERAL) })
    }

    /// The rest of the exchange as OpenSSH runs it: the server's KEXINIT is
    /// the next packet, and its signature over `H` is verified here.
    pub fn finish_exchange(&mut self, exchange: Exchange) -> Result<(), Refusal> {
        let x25519_public = exchange.private.public_key().0;
        let q_c = match &exchange.kem {
            Some(kem) => [&kem.public_key().as_bytes()[..], &x25519_public].concat(),
            None => x25519_public.to_vec(),
        };
        let mut init = vec![30];
        string(&mut init, &q_c);
        self.send(&init)?;
        let i_s = self.next().expect("the server's KEXINIT");
        assert_eq!(i_s[0], 20);
        let reply = self.next().expect("the exchange's reply");
        let mut f = Fields(&reply);
        assert_eq!(f.byte(), 31);
        let (k_s, q_s, signature) = (f.string().to_vec(), f.string().to_vec(), f.string().to_vec());
        assert_eq!(self.next().expect("NEWKEYS"), [21]);
        let (ciphertext, server_x25519) = q_s.split_at(q_s.len() - 32);
        let k_cl = exchange.private.agree(&x25519::PublicKey(server_x25519.try_into().unwrap())).unwrap();
        let k = match exchange.kem {
            Some(kem) => hybrid_k(kem.decapsulate(ciphertext).unwrap().as_bytes(), k_cl.as_bytes()),
            None => curve25519_k(k_cl.as_bytes()),
        };
        let k = &k[..];
        let h = exchange_hash(VERSION, &self.server_version, &exchange.i_c, &i_s, &k_s, &q_c, &q_s, k);
        let host = Fields(&k_s).string_pair();
        let sig = Fields(&signature).string_pair();
        assert_eq!((host.0, sig.0), (&b"ssh-ed25519"[..], &b"ssh-ed25519"[..]));
        let host = ed25519::PublicKey::from_bytes(host.1.try_into().unwrap()).unwrap();
        host.verify(&h, sig.1).expect("the server signed H");
        let id = *self.session_id.get_or_insert(h);
        self.opening = Some(OpeningKey::new(derive(k, &h, b'D', &id)));
        self.in_seq = 0;
        self.send(&[21])?;
        self.sealing = Some(SealingKey::new(derive(k, &h, b'C', &id), 0));
        Ok(())
    }

    /// A first key exchange with `kex` as the client's method list.
    pub fn handshake_with(server: Server<Keys, R>, kex: &str) -> Result<Self, Refusal> {
        let mut c = Self::new(server)?;
        let exchange = c.start_exchange(kex)?;
        c.finish_exchange(exchange)?;
        Ok(c)
    }

    pub fn handshake(server: Server<Keys, R>) -> Result<Self, Refusal> {
        Self::handshake_with(server, &format!("{HYBRID},{CURVE25519},ext-info-c,{STRICT}"))
    }

    pub fn session_id(&self) -> [u8; 32] {
        self.session_id.expect("an exchange is done")
    }

    /// Ask for `ssh-userauth`.
    pub fn service(&mut self) -> Result<(), Refusal> {
        let mut request = vec![5];
        string(&mut request, b"ssh-userauth");
        self.send(&request)
    }

    /// A `publickey` request signed by `pair`, as OpenSSH sends it.
    pub fn sign_in(&mut self, user: &str, pair: &SigningKey) -> Result<(), Refusal> {
        let (request, signed) = publickey_request(&self.session_id(), user, "ssh-ed25519", &ed25519_blob(pair.public_key().as_bytes()));
        self.send(&signed_request(&request, pair, &signed))
    }
}


impl<'a> Fields<'a> {
    pub fn string_pair(&mut self) -> (&'a [u8], &'a [u8]) {
        (self.string(), self.string())
    }
}

/// An `ssh-ed25519` public key blob.
pub fn ed25519_blob(public: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    string(&mut out, b"ssh-ed25519");
    string(&mut out, public);
    out
}

/// A `publickey` USERAUTH_REQUEST, and the bytes a signature over it covers.
pub fn publickey_request(session_id: &[u8; 32], user: &str, algorithm: &str, blob: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let mut request = vec![50];
    string(&mut request, user.as_bytes());
    string(&mut request, b"ssh-connection");
    string(&mut request, b"publickey");
    request.push(1);
    string(&mut request, algorithm.as_bytes());
    string(&mut request, blob);
    let mut signed = Vec::new();
    string(&mut signed, session_id);
    signed.extend_from_slice(&request);
    (request, signed)
}

/// A signed `publickey` request by `pair`, whose signature covers `signed`.
pub fn signed_request(request: &[u8], pair: &SigningKey, signed: &[u8]) -> Vec<u8> {
    let mut sig = Vec::new();
    string(&mut sig, b"ssh-ed25519");
    string(&mut sig, &pair.sign(signed).unwrap());
    let mut out = request.to_vec();
    string(&mut out, &sig);
    out
}

/// A key pair from a seed, and its public half, made once per seed: the fuzz
/// asks for the same two at every message.
pub fn pair(seed: u8) -> &'static (SigningKey, [u8; 32]) {
    static PAIRS: [OnceLock<(SigningKey, [u8; 32])>; 256] = [const { OnceLock::new() }; 256];
    PAIRS[usize::from(seed)].get_or_init(|| {
        let pair = SigningKey::from_seed([seed; 32]);
        let public = *pair.public_key().as_bytes();
        (pair, public)
    })
}
