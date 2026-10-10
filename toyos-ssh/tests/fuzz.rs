//! **Structured fuzzing of the boundary**, with a fixed count of seeded
//! inputs, so a red names its iteration.
//!
//! Two shapes. Sessions `common::client` holds with the server, sealed and
//! well framed, whose every message after the exchange is drawn from the
//! protocol's grammar — every authentication method and key, signatures right
//! and wrong, channel traffic before and after authentication, transport
//! messages, random bytes — and is then sometimes truncated, bent, lengthened
//! or given a huge length. And the OpenSSH recordings, their client bytes cut,
//! flipped, spliced and fed in random splits.
//!
//! Every input ends in progress or in a named refusal, never a panic, and
//! after a refusal every input is refused. **No session is authenticated but
//! by a request this file signed rightly with the key the authorizer names**:
//! the fuzz keeps that record itself, so a server that skipped the signature
//! check reds here.

mod common;

use std::collections::BTreeMap;

use common::client::{ed25519_blob, open_packet, pair, publickey_request, signed_request, string, Client};
use common::driver::{host_key, public_blob, Keys, Side, Transcript, USER};
use ring::rand::SystemRandom;
use toyos_ssh::{Event, Refusal, Server};

const FUZZ_ITERATIONS: usize = 4_000;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }

    fn bytes(&mut self, max: usize) -> Vec<u8> {
        let len = self.below(max + 1);
        (0..len).map(|_| self.next() as u8).collect()
    }
}

/// One message of the grammar, and whether it is a request this file signed
/// rightly with the named key for the named user.
fn message(rng: &mut Rng, session_id: &[u8; 32]) -> (Vec<u8>, bool) {
    let (good, good_public) = pair(1);
    let (stranger, stranger_public) = pair(2);
    let good_blob = ed25519_blob(&good_public);
    let channel = || -> [u8; 4] { [0, 0, 0, 0] };
    let mut out = Vec::new();
    let mut valid = false;
    match rng.below(26) {
        0 => {
            out.push(5);
            string(&mut out, b"ssh-userauth");
        }
        1 => {
            out.push(5);
            string(&mut out, b"ssh-connection");
        }
        2 => {
            out.push(50);
            string(&mut out, USER.as_bytes());
            string(&mut out, b"ssh-connection");
            string(&mut out, b"none");
        }
        3 => {
            out.push(50);
            string(&mut out, USER.as_bytes());
            string(&mut out, b"ssh-connection");
            string(&mut out, [&b"password"[..], b"keyboard-interactive", b"hostbased"][rng.below(3)]);
            out.extend(rng.bytes(16));
        }
        4..=6 => {
            let (algorithm, blob): (&str, Vec<u8>) = match rng.below(3) {
                0 => ("ssh-ed25519", good_blob),
                1 => ("ssh-ed25519", ed25519_blob(&stranger_public)),
                _ => (["rsa-sha2-512", "ssh-rsa"][rng.below(2)], public_blob(include_str!("fixtures/user_rsa.pub"))),
            };
            out.push(50);
            string(&mut out, USER.as_bytes());
            string(&mut out, b"ssh-connection");
            string(&mut out, b"publickey");
            out.push(0);
            string(&mut out, algorithm.as_bytes());
            string(&mut out, &blob);
        }
        7..=12 => {
            let user = if rng.chance(85) { USER } else { "root" };
            let (request, signed) = publickey_request(session_id, user, "ssh-ed25519", &good_blob);
            out = match rng.below(6) {
                0 | 1 => {
                    valid = user == USER;
                    signed_request(&request, &good, &signed)
                }
                2 => {
                    let mut other = *session_id;
                    other[rng.below(32)] ^= 1 << rng.below(8);
                    signed_request(&request, &good, &publickey_request(&other, user, "ssh-ed25519", &good_blob).1)
                }
                3 => signed_request(&request, &stranger, &signed),
                4 => {
                    let (request, signed) = publickey_request(session_id, user, "ssh-ed25519", &ed25519_blob(&stranger_public));
                    signed_request(&request, &stranger, &signed)
                }
                _ => {
                    let mut bent = signed_request(&request, &good, &signed);
                    let at = bent.len() - 1 - rng.below(64);
                    bent[at] ^= 1 << rng.below(8);
                    bent
                }
            };
        }
        13 | 14 => {
            out.push(90);
            string(&mut out, if rng.chance(70) { b"session" } else { b"direct-tcpip" });
            out.extend_from_slice(&(rng.below(4) as u32).to_be_bytes());
            out.extend_from_slice(&(rng.next() as u32).to_be_bytes());
            out.extend_from_slice(&(rng.below(70_000) as u32).to_be_bytes());
        }
        15 | 16 => {
            out.push(98);
            out.extend_from_slice(&(rng.below(3) as u32).to_be_bytes());
            string(&mut out, [&b"exec"[..], b"shell", b"pty-req", b"subsystem"][rng.below(4)]);
            out.push(rng.below(2) as u8);
            string(&mut out, &rng.bytes(24));
        }
        17 => {
            out.push(94);
            out.extend_from_slice(&(rng.below(3) as u32).to_be_bytes());
            string(&mut out, &rng.bytes(300));
        }
        18 => {
            out.push([96, 97][rng.below(2)]);
            out.extend_from_slice(&(rng.below(3) as u32).to_be_bytes());
        }
        19 => {
            out.push(93);
            out.extend_from_slice(&channel());
            out.extend_from_slice(&(rng.next() as u32).to_be_bytes());
        }
        20 => {
            out.push(80);
            string(&mut out, b"keepalive@openssh.com");
            out.push(rng.below(2) as u8);
        }
        21 => {
            out.push([2, 4, 3][rng.below(3)]);
            out.extend(rng.bytes(12));
        }
        22 => out = common::client::kexinit("curve25519-sha256"),
        23 => {
            out.push(1);
            out.extend_from_slice(&11u32.to_be_bytes());
            string(&mut out, b"bye");
            string(&mut out, b"");
        }
        _ => {
            out.push(rng.next() as u8);
            out.extend(rng.bytes(40));
        }
    }
    (out, valid)
}

/// The opening a client makes: 0 the service, 7 a right login, 13 a session
/// channel, 15 its exec.
fn message_of(kind: usize, session_id: &[u8; 32]) -> Vec<u8> {
    let mut out = Vec::new();
    match kind {
        0 => {
            out.push(5);
            string(&mut out, b"ssh-userauth");
        }
        7 => {
            let (good, public) = pair(1);
            let (request, signed) = publickey_request(session_id, USER, "ssh-ed25519", &ed25519_blob(&public));
            out = signed_request(&request, &good, &signed);
        }
        13 => {
            out.push(90);
            string(&mut out, b"session");
            out.extend_from_slice(&[0, 0, 0, 0, 0, 0x10, 0, 0, 0, 0, 0x80, 0]);
        }
        _ => {
            out.extend_from_slice(&[98, 0, 0, 0, 0]);
            string(&mut out, b"exec");
            out.push(1);
            string(&mut out, b"true");
        }
    }
    out
}

/// Bend `payload` one way, or not at all; whether it changed.
fn mutate(rng: &mut Rng, payload: &mut Vec<u8>) -> bool {
    if !rng.chance(25) || payload.is_empty() {
        return false;
    }
    match rng.below(4) {
        0 => payload.truncate(rng.below(payload.len())),
        1 => {
            let at = rng.below(payload.len());
            payload[at] ^= 1 + rng.below(255) as u8;
        }
        2 => payload.extend(rng.bytes(8).into_iter().chain([rng.next() as u8])),
        _ => {
            let at = rng.below(payload.len());
            let huge = [0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x88, 0xb9][rng.below(2) * 4..][..4].to_vec();
            payload.splice(at..(at + 4).min(payload.len()), huge);
        }
    }
    true
}

fn tally(outcomes: &mut BTreeMap<String, usize>, result: &Result<(), Refusal>) {
    let key = match result {
        Ok(()) => "progress".to_string(),
        Err(refusal) => {
            let name = format!("{refusal:?}");
            name.split([' ', '(', '{']).next().unwrap().to_string()
        }
    };
    *outcomes.entry(key).or_default() += 1;
}

#[test]
fn every_session_is_total_and_none_logs_in_without_a_right_signature() {
    let mut outcomes = BTreeMap::new();
    let mut authenticated = 0;
    for iteration in 0..FUZZ_ITERATIONS {
        let mut rng = Rng(iteration as u64);
        let server = Server::new(host_key(), Keys::new(vec![pair(1).1]), SystemRandom::new());
        let mut c = Client::handshake(server).expect("the exchange");
        let id = c.session_id();
        let mut service = false;
        let mut signed_rightly = false;
        let mut logged_in = false;
        let mut delivered: BTreeMap<toyos_ssh::ChannelId, usize> = BTreeMap::new();
        // Most sessions open as a client does, so the draws reach every phase:
        // the service, then often a right login and a channel with its exec.
        let opening = rng.below(4);
        let mut script: Vec<Vec<u8>> = Vec::new();
        if opening >= 1 {
            script.push(message_of(0, &id));
        }
        if opening >= 2 {
            script.push(message_of(7, &id));
        }
        if opening >= 3 {
            script.push(message_of(13, &id));
            script.push(message_of(15, &id));
        }
        let steps = script.len() + 1 + rng.below(14);
        for step in 0..steps {
            let (payload, valid, bent) = match script.get(step) {
                Some(scripted) => (scripted.clone(), step == 1, false),
                None => {
                    let (mut payload, valid) = message(&mut rng, &id);
                    let bent = mutate(&mut rng, &mut payload);
                    (payload, valid, bent)
                }
            };
            let before = c.events.len();
            let result = c.send(&payload);
            signed_rightly |= valid && !bent && service;
            tally(&mut outcomes, &result);
            let replies = c.recv();
            service |= replies.iter().any(|r| r.first() == Some(&6));
            logged_in |= c.events[before..].iter().any(|e| matches!(e, Event::Authenticated { .. }));
            let authenticated_now = logged_in;
            assert!(
                !authenticated_now || signed_rightly,
                "iteration {iteration}: authenticated without a right signature after {payload:02x?}"
            );
            let new: Vec<Event> = c.events.drain(before..).collect();
            for event in new {
                match event {
                    Event::Authenticated { .. } => {
                        assert!(signed_rightly, "iteration {iteration}: an Authenticated event without a right signature");
                        authenticated += 1;
                    }
                    Event::Exec { channel, .. } | Event::Writable { channel } => {
                        assert!(authenticated_now, "iteration {iteration}: a channel event before authentication");
                        let _ = c.server.send(channel, &rng.bytes(100));
                        if rng.chance(20) {
                            let _ = c.server.exit(channel, rng.below(256) as u32);
                        }
                    }
                    Event::Data { channel, data } => {
                        assert!(authenticated_now, "iteration {iteration}: a channel event before authentication");
                        *delivered.entry(channel).or_default() += data.len();
                        if rng.chance(50) {
                            let n = delivered.remove(&channel).unwrap_or_default();
                            let _ = c.server.consumed(channel, n);
                        }
                    }
                    Event::Eof { channel } | Event::Closed { channel } => {
                        assert!(authenticated_now, "iteration {iteration}: a channel event before authentication");
                        delivered.remove(&channel);
                    }
                    Event::Declined(_) | Event::Disconnected => {}
                }
            }
            c.recv();
            if result.is_err() {
                assert_eq!(c.send(&[2, 0, 0, 0, 0]), Err(Refusal::Ended), "iteration {iteration}: input after a refusal");
                break;
            }
        }
    }
    println!("{FUZZ_ITERATIONS} sessions, {authenticated} authenticated: {outcomes:#?}");
    assert!(authenticated > FUZZ_ITERATIONS / 10, "the fuzz reaches authentication");
    assert!(outcomes.len() > 8, "the fuzz reaches many refusals");
}

/// The recorded exchange's client bytes, and where the signed request that
/// logs in ends in them: nothing before it may change and still log in, but
/// a plaintext packet's padding, which no hash covers.
struct Recording {
    seed: u8,
    chunks: Vec<Vec<u8>>,
    client: Vec<u8>,
    signed_end: usize,
    padding: Vec<std::ops::Range<usize>>,
}

impl Recording {
    fn read(text: &str) -> Self {
        let t = Transcript::read(text);
        let client = t.side(Side::Client);
        let chunks: Vec<Vec<u8>> = t.chunks.iter().filter(|(s, _)| *s == Side::Client).map(|(_, b)| b.clone()).collect();
        let mut server = Server::new(host_key(), Keys::recorded(), seeded(t.seed));
        let signed_end = (0..client.len())
            .find(|&at| {
                server.input(&client[at..at + 1]).unwrap();
                std::iter::from_fn(|| server.poll()).any(|e| matches!(e, Event::Authenticated { .. }))
            })
            .expect("the recording logs in")
            + 1;
        let mut at = client.windows(2).position(|w| w == b"\r\n").unwrap() + 2;
        let mut padding = Vec::new();
        for _ in 0..3 {
            let (_, used) = open_packet(None, 0, &client[at..]).unwrap();
            let pad = client[at + 4] as usize;
            padding.push(at + used - pad..at + used);
            at += used;
        }
        Self { seed: t.seed, chunks, client, signed_end, padding }
    }
}

#[allow(deprecated)]
fn seeded(seed: u8) -> ring::test::rand::FixedByteRandom {
    ring::test::rand::FixedByteRandom { byte: seed }
}

#[test]
fn every_bent_recording_is_total_and_a_bend_before_the_login_prevents_it() {
    let recordings = [
        Recording::read(include_str!("fixtures/exec.transcript")),
        Recording::read(include_str!("fixtures/stdin.transcript")),
    ];
    let mut outcomes = BTreeMap::new();
    let mut held = 0;
    for iteration in 0..FUZZ_ITERATIONS {
        let mut rng = Rng(1 << 32 | iteration as u64);
        let r = &recordings[rng.below(recordings.len())];
        let mut bytes = r.client.clone();
        let at = if rng.chance(70) { rng.below(r.signed_end) } else { rng.below(bytes.len()) };

        let flip = match rng.below(5) {
            0 => {
                bytes.truncate(at);
                false
            }
            1 => {
                let n = 1 + rng.below(16);
                bytes.splice(at..at, rng.bytes(n).into_iter().chain([0x5a]));
                false
            }
            2 => {
                let end = (at + 1 + rng.below(64)).min(bytes.len());
                bytes.drain(at..end);
                false
            }
            3 => {
                let end = (at + 1 + rng.below(64)).min(bytes.len());
                let copy = bytes[at..end].to_vec();
                bytes.splice(end..end, copy);
                false
            }
            _ => {
                bytes[at] ^= 1 + rng.below(255) as u8;
                true
            }
        };
        let mut server = Server::new(host_key(), Keys::recorded(), seeded(r.seed));
        let mut result = Ok(());
        let mut logged_in = false;
        let mut rest = &bytes[..];
        while !rest.is_empty() && result.is_ok() {
            let n = (1 + rng.below(2 * r.chunks.len().max(1) * 64)).min(rest.len());
            result = server.input(&rest[..n]);
            rest = &rest[n..];
            logged_in |= answer(&mut server);
            server.output();
        }
        tally(&mut outcomes, &result);
        // The first byte the bend changed, whatever its kind.
        let changed = bytes.iter().zip(&r.client).position(|(a, b)| a != b).unwrap_or(bytes.len().min(r.client.len()));
        let in_padding = r.padding.iter().any(|p| p.contains(&changed));
        if changed < r.signed_end && !(flip && in_padding) {
            assert!(!logged_in, "iteration {iteration}: a bend at byte {changed} before the login still logged in");
            held += 1;
        }
        if result.is_err() {
            assert_eq!(server.input(b"x"), Err(Refusal::Ended));
        }
    }
    println!("{FUZZ_ITERATIONS} bent recordings, {held} bent before the login: {outcomes:#?}");
    assert!(held > FUZZ_ITERATIONS / 2);
}

/// The scripted driver's answers, each taken whatever the server says to it
/// (a bent session can end between an event and its answer); whether the
/// session authenticated.
fn answer<R: ring::rand::SecureRandom>(server: &mut Server<Keys, R>) -> bool {
    let mut authenticated = false;
    while let Some(event) = server.poll() {
        match event {
            Event::Exec { channel, command } => {
                let _ = server.send(channel, &command);
            }
            Event::Data { channel, data } => {
                let _ = server.send(channel, &data);
                let _ = server.consumed(channel, data.len());
            }
            Event::Eof { channel } => {
                let _ = server.exit(channel, 3);
            }
            Event::Authenticated { .. } => authenticated = true,
            Event::Closed { .. } | Event::Writable { .. } | Event::Declined(_) | Event::Disconnected => {}
        }
    }
    authenticated
}
