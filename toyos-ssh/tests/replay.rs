//! **The independent oracle**: sessions OpenSSH's own `ssh` (OpenSSH_10.3p1)
//! held with this server, replayed byte for byte.
//!
//! `examples/record.rs` served each with the fixture host key, the scripted
//! driver and a fixed-byte randomness, so the server's ephemeral key, cookie
//! and padding come again from the transcript's seed, and its every output
//! byte must come again too. OpenSSH accepted those bytes: it verified the host
//! key against a pinned `known_hosts` entry and the signature over `H`, opened
//! every packet, and exited with the status the driver sent. Its own bytes —
//! sealed under the keys it derived — open here only under the keys this server
//! derives. Nothing runs `ssh` at test time.
//!
//! Beside the replay, the keys are derived a second time from the recorded
//! exchange by this file's own code (`common::client`), and must open what
//! OpenSSH sealed at the sequence numbers strict key exchange resets to.

mod common;

use common::client::{derive, exchange_hash, open_packet, plain_packet, Fields};
use common::driver::{drive, host_key, public_key, Keys, Side, Transcript, STATUS, USER};
use ring::aead::chacha20_poly1305_openssh::{OpeningKey, TAG_LEN};
use toyos_ssh::{Declined, Event, Refusal, Server};

const EXEC: &str = include_str!("fixtures/exec.transcript");
const STDIN: &str = include_str!("fixtures/stdin.transcript");
const REKEY: &str = include_str!("fixtures/rekey.transcript");
const STRANGER: &str = include_str!("fixtures/stranger.transcript");
const RSA: &str = include_str!("fixtures/rsa.transcript");
const FORWARD: &str = include_str!("fixtures/forward.transcript");

#[allow(deprecated)]
fn seeded(seed: u8) -> ring::test::rand::FixedByteRandom {
    ring::test::rand::FixedByteRandom { byte: seed }
}

/// Feed `chunks` to a server seeded with `seed`, each in `piece`-byte inputs
/// and the driver run after each whole chunk, as the recorder ran it: the
/// server's output, its events and how the last input ended.
fn run_split(seed: u8, chunks: &[&[u8]], piece: usize) -> (Vec<u8>, Vec<Event>, Result<(), Refusal>) {
    let mut server = Server::new(host_key(), Keys::recorded(), seeded(seed));
    let mut out = server.output();
    let mut events = Vec::new();
    for chunk in chunks {
        let result = chunk.chunks(piece).try_for_each(|piece| server.input(piece));
        events.extend(drive(&mut server));
        out.extend(server.output());
        if result.is_err() {
            return (out, events, result);
        }
    }
    (out, events, Ok(()))
}

fn run(seed: u8, chunks: &[&[u8]]) -> (Vec<u8>, Vec<Event>, Result<(), Refusal>) {
    run_split(seed, chunks, usize::MAX)
}

/// The transcript replayed as recorded, and with every recorded chunk cut at
/// every byte and at every 61st: each makes every byte the recorded server sent.
fn replay(text: &str) -> Vec<Event> {
    let t = Transcript::read(text);
    let recorded: Vec<&[u8]> = t.chunks.iter().filter(|(s, _)| *s == Side::Client).map(|(_, b)| &b[..]).collect();
    let want = t.side(Side::Server);
    let (out, events, result) = run(t.seed, &recorded);
    assert_eq!(result, Ok(()));
    if out != want {
        let at = out.iter().zip(&want).position(|(a, b)| a != b).unwrap_or(out.len().min(want.len()));
        panic!("the output parts from the recording at byte {at} of {} (recorded {})", out.len(), want.len());
    }
    for piece in [1, 61] {
        let (again, again_events, result) = run_split(t.seed, &recorded, piece);
        assert_eq!(result, Ok(()));
        assert!(again == want, "the recorded chunks cut every {piece} bytes make other output");
        assert_eq!(again_events, events);
    }
    events
}

fn data(events: &[Event]) -> Vec<u8> {
    events.iter().filter_map(|e| if let Event::Data { data, .. } = e { Some(&data[..]) } else { None }).flatten().copied().collect()
}

fn lines(n: u32) -> Vec<u8> {
    (1..=n).flat_map(|i| format!("{i}\n").into_bytes()).collect()
}

#[test]
fn an_exec_openssh_ran_replays() {
    let events = replay(EXEC);
    assert!(matches!(&events[..], [
        Event::Authenticated { user },
        Event::Exec { command, .. },
        Event::Eof { .. },
        Event::Disconnected,
    ] if user == USER && command == b"echo hello"));
    assert_eq!(STATUS, 3, "the recording's client exited 3: it read the exit status");
}

#[test]
fn an_exec_fed_stdin_replays() {
    let events = replay(STDIN);
    assert!(matches!(&events[..2], [Event::Authenticated { .. }, Event::Exec { command, .. }] if command == b"cat"));
    assert_eq!(data(&events), lines(2000));
    assert!(matches!(&events[events.len() - 2..], [Event::Eof { .. }, Event::Disconnected]));
}

/// `RekeyLimit=4K`: the client asked for three re-exchanges in the middle of
/// its standard input, each one's packets around the channel's.
#[test]
fn a_session_the_client_rekeyed_three_times_replays() {
    let events = replay(REKEY);
    assert_eq!(data(&events), lines(3000));
    assert!(matches!(events.last(), Some(Event::Disconnected)));
}

/// The offer of a key the authorizer does not name is refused, so `ssh` never
/// signed with it, and left without a word.
#[test]
fn a_key_the_server_does_not_name_is_refused_at_the_offer() {
    let events = replay(STRANGER);
    let stranger = toyos_ssh::hostkey::fingerprint(&public_key(include_str!("fixtures/stranger_ed25519.pub")));
    assert_eq!(events, [Event::Declined(Declined::Key { user: USER.into(), fingerprint: stranger })]);
}

/// OpenSSH never offers an RSA key to a server whose `server-sig-algs` names
/// only `ssh-ed25519`: it says "Permission denied" without one on the wire.
/// The offer itself is refused by name in `refusals.rs`.
#[test]
fn openssh_holding_only_an_rsa_key_offers_none() {
    assert_eq!(replay(RSA), []);
}

/// `ssh -W` opens a `direct-tcpip` channel, as `-L` does on its first
/// connection: refused by name, which `ssh` reported as "administratively
/// prohibited" and left.
#[test]
fn a_forwarding_channel_is_refused_by_name() {
    let events = replay(FORWARD);
    assert!(matches!(&events[..], [
        Event::Authenticated { .. },
        Event::Declined(Declined::ChannelType(kind)),
    ] if kind == "direct-tcpip"));
}

/// The recording replayed under another seed: the server's ephemeral key is
/// another, so the client's first sealed packet does not open: its length,
/// decrypted under another key, is refused before its tag is read. The replay
/// is not one that would pass whatever the keys.
#[test]
fn a_replay_under_another_seed_does_not_open() {
    let t = Transcript::read(EXEC);
    let (_, events, result) = run(t.seed ^ 1, &[&t.side(Side::Client)]);
    assert_eq!(events, []);
    assert!(matches!(result, Err(Refusal::SealedLength { .. } | Refusal::Integrity)), "{result:?}");
}

/// A recorded session's plaintext exchange, parsed by this test.
struct Recorded {
    client: Vec<u8>,
    server: Vec<u8>,
    /// Where each side's sealed packets start.
    client_sealed: usize,
    server_sealed: usize,
    /// Where the client's KEXINIT, KEX_ECDH_INIT and NEWKEYS start.
    client_packets: [usize; 3],
    k: Vec<u8>,
    h: [u8; 32],
}

impl Recorded {
    fn read(text: &str) -> Self {
        let t = Transcript::read(text);
        let (client, server) = (t.side(Side::Client), t.side(Side::Server));
        let line = |bytes: &[u8]| bytes.windows(2).position(|w| w == b"\r\n").unwrap();
        let (v_c, v_s) = (&client[..line(&client)], &server[..line(&server)]);
        let mut at = (v_c.len() + 2, v_s.len() + 2);
        let mut client_packets = [0; 3];
        let mut c = Vec::new();
        let mut s = Vec::new();
        for slot in &mut client_packets {
            *slot = at.0;
            let (payload, used) = open_packet(None, 0, &client[at.0..]).unwrap();
            c.push(payload);
            at.0 += used;
            let (payload, used) = open_packet(None, 0, &server[at.1..]).unwrap();
            s.push(payload);
            at.1 += used;
        }
        assert_eq!((c[0][0], c[1][0], c[2][..].to_vec()), (20, 30, vec![21]));
        assert_eq!((s[0][0], s[1][0], s[2][..].to_vec()), (20, 31, vec![21]));
        let q_c = Fields(&c[1][1..]).string().to_vec();
        let mut reply = Fields(&s[1][1..]);
        let (k_s, q_s, signature) = (reply.string().to_vec(), reply.string().to_vec(), reply.string().to_vec());
        // The server's ephemeral key is the seed's: generated here as the
        // server generates it, it is the Q_S the recording carries.
        let private = ring::agreement::EphemeralPrivateKey::generate(&ring::agreement::X25519, &seeded(t.seed)).unwrap();
        assert_eq!(private.compute_public_key().unwrap().as_ref(), &q_s[..]);
        let k = ring::agreement::agree_ephemeral(
            private,
            &ring::agreement::UnparsedPublicKey::new(&ring::agreement::X25519, &q_c),
            |k| k.to_vec(),
        )
        .unwrap();
        let h = exchange_hash(v_c, v_s, &c[0], &s[0], &k_s, &q_c, &q_s, &k);
        let (host, sig) = (Fields(&k_s).string_pair().1.to_vec(), Fields(&signature).string_pair().1.to_vec());
        ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, host).verify(&h, &sig).expect("the recorded signature covers H");
        Self { client_sealed: at.0, server_sealed: at.1, client, server, client_packets, k, h }
    }

    fn key(&self, letter: u8) -> OpeningKey {
        OpeningKey::new(&derive(&self.k, &self.h, letter, &self.h))
    }

    /// The length of the sealed packet at `at`, under `key` at `seq`.
    fn sealed_len(bytes: &[u8], key: &OpeningKey, seq: u32) -> usize {
        4 + u32::from_be_bytes(key.decrypt_packet_length(seq, bytes[..4].try_into().unwrap())) as usize + TAG_LEN
    }
}

/// **Key derivation against OpenSSH's.** The exchange hash and the keys of
/// RFC 4253 §7.2, computed here from the recorded exchange, open the first
/// packets OpenSSH sealed, at sequence number zero — strict key exchange's
/// reset — and the first the server sealed.
#[test]
fn the_keys_derived_from_a_recorded_exchange_open_what_openssh_sealed() {
    let r = Recorded::read(EXEC);
    let c = r.key(b'C');
    let (service, used) = open_packet(Some(&c), 0, &r.client[r.client_sealed..]).unwrap();
    let mut want = vec![5];
    common::client::string(&mut want, b"ssh-userauth");
    assert_eq!(service, want);
    let (auth, _) = open_packet(Some(&c), 1, &r.client[r.client_sealed + used..]).unwrap();
    assert_eq!(auth[0], 50, "USERAUTH_REQUEST");
    let (info, _) = open_packet(Some(&r.key(b'D')), 0, &r.server[r.server_sealed..]).unwrap();
    let mut f = Fields(&info);
    assert_eq!((f.byte(), f.u32(), f.string(), f.string()), (7, 1, &b"server-sig-algs"[..], &b"ssh-ed25519"[..]));
    // Under the count of packets before NEWKEYS, which a client without
    // strict key exchange would have used, it does not open.
    let mut packet = r.client[r.client_sealed..r.client_sealed + used].to_vec();
    let (sealed, tag) = packet.split_at_mut(used - TAG_LEN);
    assert!(c.open_in_place(3, sealed, (&*tag).try_into().unwrap()).is_err());
}

/// **A prefix-truncation attack** (Terrapin, CVE-2023-48795): an `IGNORE`
/// slipped into the client's plaintext before its NEWKEYS, and its first
/// sealed packet removed to even the count. Refused at the `IGNORE`, by name,
/// before the session reads anything sealed.
#[test]
fn a_terrapin_injection_into_the_first_exchange_is_refused() {
    let r = Recorded::read(EXEC);
    let first = Recorded::sealed_len(&r.client[r.client_sealed..], &r.key(b'C'), 0);
    let newkeys = r.client_packets[2];
    let attack = [
        &r.client[..newkeys],
        &plain_packet(&[2, 0, 0, 0, 0]),
        &r.client[newkeys..r.client_sealed],
        &r.client[r.client_sealed + first..],
    ]
    .concat();
    let t = Transcript::read(EXEC);
    let (_, events, result) = run(t.seed, &[&attack]);
    assert_eq!((events, result), (vec![], Err(Refusal::Unexpected { phase: "the first key exchange", message: 2 })));
    // Without the removal, the injection alone is refused the same way.
    let injected = [&r.client[..newkeys], &plain_packet(&[2, 0, 0, 0, 0]), &r.client[newkeys..]].concat();
    assert_eq!(run(t.seed, &[&injected]).2, Err(Refusal::Unexpected { phase: "the first key exchange", message: 2 }));
}
