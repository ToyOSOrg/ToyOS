//! **The boundary, message by message**: every refusal the server makes, each
//! by its name, driven by `common::client` — a client written from the RFCs
//! beside the server, which can say what OpenSSH never would.

mod common;

use common::client::{ed25519_blob, pair, plain_packet, publickey_request, signed_request, string, Client, Fields, STRICT};
use common::driver::{host_key, public_blob, Keys, USER};
use ring::rand::SystemRandom;
use ring::signature::KeyPair;
use toyos_ssh::{Declined, Event, Gone, Refusal, Server};

type C = Client<SystemRandom>;

fn server() -> Server<Keys, SystemRandom> {
    Server::new(host_key(), Keys::new(vec![pair(1).1]), SystemRandom::new())
}

fn handshake() -> C {
    let mut c = C::handshake(server()).expect("the exchange");
    let info = c.recv();
    assert_eq!(info.len(), 1);
    assert_eq!(info[0][0], 7, "EXT_INFO follows the first NEWKEYS");
    c
}

fn with_service() -> C {
    let mut c = handshake();
    c.service().unwrap();
    let mut accept = vec![6];
    string(&mut accept, b"ssh-userauth");
    assert_eq!(c.recv(), [accept]);
    c
}

fn logged_in() -> C {
    let mut c = with_service();
    c.sign_in(USER, &pair(1).0).unwrap();
    assert_eq!(c.recv(), [vec![52]]);
    assert_eq!(c.events, [Event::Authenticated { user: USER.into() }]);
    c.events.clear();
    c
}

fn authenticated(c: &C) -> bool {
    c.events.iter().any(|e| matches!(e, Event::Authenticated { .. }))
}

fn failure() -> Vec<u8> {
    let mut out = vec![51];
    string(&mut out, b"publickey");
    out.push(0);
    out
}

fn request(user: &str, method: &str, rest: &[u8]) -> Vec<u8> {
    let mut out = vec![50];
    string(&mut out, user.as_bytes());
    string(&mut out, b"ssh-connection");
    string(&mut out, method.as_bytes());
    out.extend_from_slice(rest);
    out
}

fn probe(algorithm: &str, blob: &[u8]) -> Vec<u8> {
    let mut rest = vec![0];
    string(&mut rest, algorithm.as_bytes());
    string(&mut rest, blob);
    request(USER, "publickey", &rest)
}

/// What the client's DISCONNECT says: its reason code and text.
fn disconnect(payloads: &[Vec<u8>]) -> (u32, String) {
    let [payload] = payloads else { panic!("one DISCONNECT, not {payloads:?}") };
    let mut f = Fields(payload);
    assert_eq!(f.byte(), 1);
    (f.u32(), String::from_utf8(f.string().to_vec()).unwrap())
}

#[test]
fn a_listed_key_is_asked_to_sign_and_its_signature_logs_in() {
    let mut c = with_service();
    let blob = ed25519_blob(pair(1).0.public_key().as_ref());
    c.send(&probe("ssh-ed25519", &blob)).unwrap();
    let mut ok = vec![60];
    string(&mut ok, b"ssh-ed25519");
    string(&mut ok, &blob);
    assert_eq!(c.recv(), [ok]);
    assert!(!authenticated(&c));
    c.sign_in(USER, &pair(1).0).unwrap();
    assert_eq!(c.recv(), [vec![52]]);
    assert!(authenticated(&c));
}

/// **The RSA refusal**: an offer of an RSA key, under each of its names, is
/// refused by the algorithm's name and never asked to sign.
#[test]
fn an_rsa_key_is_refused_by_name() {
    let blob = public_blob(include_str!("fixtures/user_rsa.pub"));
    for algorithm in ["rsa-sha2-512", "rsa-sha2-256", "ssh-rsa"] {
        let mut c = with_service();
        c.send(&probe(algorithm, &blob)).unwrap();
        assert_eq!(c.recv(), [failure()]);
        assert_eq!(c.events, [Event::Declined(Declined::KeyAlgorithm(algorithm.into()))]);
    }
}

/// Every method but `publickey` is refused by its name; `none` as the first
/// request, the client's question of what may continue, is answered without
/// counting, and every later one is a failure.
#[test]
fn every_other_method_is_refused_by_name() {
    let mut c = with_service();
    c.send(&request(USER, "none", &[])).unwrap();
    assert_eq!(c.recv(), [failure()]);
    assert_eq!(c.events, []);
    for method in ["none", "password", "keyboard-interactive", "hostbased", "gssapi-with-mic"] {
        c.send(&request(USER, method, &[0, 0, 0, 0])).unwrap();
        assert_eq!(c.recv(), [failure()]);
        assert_eq!(c.events.pop(), Some(Event::Declined(Declined::Method(method.into()))));
    }
}

/// A key the authorizer does not name is refused at the offer, and a signed
/// request with it is refused too: it is never verified.
#[test]
fn an_unlisted_key_is_refused_at_the_offer_and_signed() {
    let (stranger, public) = pair(2);
    let mut c = with_service();
    c.send(&probe("ssh-ed25519", &ed25519_blob(&public))).unwrap();
    assert_eq!(c.recv(), [failure()]);
    c.sign_in(USER, &stranger).unwrap();
    assert_eq!(c.recv(), [failure()]);
    let declined = Event::Declined(Declined::Key { user: USER.into(), fingerprint: toyos_ssh::hostkey::fingerprint(&public) });
    assert_eq!(c.events, [declined.clone(), declined]);
    // The authorizer is asked for the user too: another user's key is not
    // this one's.
    c.events.clear();
    c.sign_in("root", &pair(1).0).unwrap();
    assert!(matches!(&c.events[..], [Event::Declined(Declined::Key { user, .. })] if user == "root"));
    assert!(!authenticated(&c));
}

/// A signature that is not this session's, this user's or this key's over
/// these bytes is refused, and logs in nobody.
#[test]
fn a_signature_over_anything_else_is_refused() {
    let (key, public) = pair(1);
    let blob = ed25519_blob(&public);
    let mut c = with_service();
    let id = c.session_id();
    let (req, signed) = publickey_request(&id, USER, "ssh-ed25519", &blob);
    let mut other_session = id;
    other_session[0] ^= 1;
    let (_, signed_elsewhere) = publickey_request(&other_session, USER, "ssh-ed25519", &blob);
    let (_, signed_for_root) = publickey_request(&id, "root", "ssh-ed25519", &blob);
    let mut bent = signed_request(&req, &key, &signed);
    *bent.last_mut().unwrap() ^= 1;
    let attempts = [
        signed_request(&req, &key, &signed_elsewhere),
        signed_request(&req, &key, &signed_for_root),
        signed_request(&req, &pair(2).0, &signed),
        bent,
    ];
    for attempt in &attempts {
        c.send(attempt).unwrap();
        assert_eq!(c.recv(), [failure()]);
        assert_eq!(c.events.pop(), Some(Event::Declined(Declined::Signature { user: USER.into() })));
    }
    assert!(!authenticated(&c));
}

/// **Six failures end the session**, with the reason RFC 4250 names, and the
/// driver is told of the sixth as of the others. A `none` after the first
/// request counts, so asking it again holds no session open.
#[test]
fn six_failures_end_the_session() {
    for method in ["password", "none"] {
        let mut c = with_service();
        c.send(&request(USER, "none", &[])).unwrap();
        assert_eq!(c.recv(), [failure()]);
        for _ in 0..5 {
            c.send(&request(USER, method, &[0, 0, 0, 0, 0])).unwrap();
            assert_eq!(c.recv(), [failure()]);
        }
        assert_eq!(c.send(&request(USER, method, &[0, 0, 0, 0, 0])), Err(Refusal::TooManyAttempts));
        assert_eq!(disconnect(&c.recv()), (14, "too many failed authentication attempts".into()));
        assert_eq!(c.events, vec![Event::Declined(Declined::Method(method.into())); 6]);
        assert_eq!(c.send(&request(USER, "none", &[])), Err(Refusal::Ended));
    }
}

/// **Nothing but authentication before it**: a channel, a global request or
/// an authentication request before the service is refused, by number.
#[test]
fn nothing_but_authentication_is_taken_before_it() {
    let mut open = vec![90];
    string(&mut open, b"session");
    open.extend_from_slice(&[0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0x80, 0]);
    let mut global = vec![80];
    string(&mut global, b"keepalive@openssh.com");
    global.push(1);
    for (service, message, number) in [(true, &open, 90), (true, &global, 80), (false, &request(USER, "none", &[]), 50)] {
        let mut c = if service { with_service() } else { handshake() };
        assert_eq!(c.send(message), Err(Refusal::Unexpected { phase: "authentication", message: number }));
        assert_eq!(disconnect(&c.recv()).0, 2);
        assert_eq!(c.events, []);
    }
    let mut c = with_service();
    assert_eq!(c.service(), Err(Refusal::Unexpected { phase: "authentication", message: 5 }));
    let mut c = handshake();
    let mut other = vec![5];
    string(&mut other, b"ssh-connection");
    assert_eq!(c.send(&other), Err(Refusal::Malformed("a service other than ssh-userauth before authentication")));
}

/// **Strict key exchange or nothing**: a client that does not offer it, or
/// offers nothing the server runs, is refused, and so is a first packet that
/// is not its KEXINIT.
#[test]
fn the_first_exchange_is_held_to_its_rules() {
    let attempt = |kex: &str| C::handshake_with(server(), kex).err();
    assert_eq!(attempt("curve25519-sha256,ext-info-c"), Some(Refusal::NotStrict));
    assert_eq!(attempt(&format!("diffie-hellman-group14-sha256,{STRICT}")), Some(Refusal::NoCommonAlgorithm("kex_algorithms")));
    assert_eq!(attempt(&format!("curve25519-sha256@libssh.org,{STRICT}")), Some(Refusal::NoCommonAlgorithm("kex_algorithms")));
    let mut c = C::new(server()).unwrap();
    assert_eq!(c.send(&[2, 0, 0, 0, 0]), Err(Refusal::Unexpected { phase: "the first key exchange", message: 2 }));
    // A second KEXINIT, and a guessed packet, are refused.
    let mut c = C::new(server()).unwrap();
    c.start_exchange(&format!("curve25519-sha256,{STRICT}")).unwrap();
    assert_eq!(c.send(&common::client::kexinit(STRICT)), Err(Refusal::Unexpected { phase: "the first key exchange", message: 20 }));
    let mut guess = common::client::kexinit(&format!("curve25519-sha256,{STRICT}"));
    let at = guess.len() - 5;
    guess[at] = 1;
    let mut c = C::new(server()).unwrap();
    assert_eq!(c.send(&guess), Err(Refusal::Malformed("a guessed key exchange packet follows the KEXINIT")));
}

/// The identification line: `SSH-2.0-`, printable, CR LF, 255 bytes at most.
#[test]
fn an_identification_line_is_held_to_rfc_4253() {
    let feed = |bytes: &[u8]| server().input(bytes);
    assert_eq!(feed(b"SSH-1.99-old\r\n"), Err(Refusal::Malformed("an identification line that is not SSH-2.0")));
    assert_eq!(feed(b"SSH-2.0-a\x01b\r\n"), Err(Refusal::Malformed("an identification line that is not printable US-ASCII")));
    assert_eq!(feed(&[b'S'; 255]), Err(Refusal::TooLong { field: "identification line", len: 255, cap: 255 }));
    assert_eq!(feed(&[b'S'; 254]), Ok(()));
    let long = [&b"SSH-2.0-"[..], &[b'x'; 246], b"\r\n"].concat();
    assert_eq!(feed(&long), Err(Refusal::TooLong { field: "identification line", len: 256, cap: 255 }));
    assert_eq!(feed(&long[2..]), Err(Refusal::Malformed("an identification line that is not SSH-2.0")));
    // A refusal before the client spoke SSH sends it no packet.
    let mut s = server();
    s.output();
    let _ = s.input(b"HTTP/1.1\r\n");
    assert_eq!(s.output(), b"");
}

/// **Packet framing**: the length cap before authentication and after, the
/// block, and the padding.
#[test]
fn a_packet_is_held_to_its_caps() {
    let mut c = C::new(server()).unwrap();
    assert_eq!(c.raw(&35_004u32.to_be_bytes()), Err(Refusal::TooLong { field: "packet_length", len: 35_004, cap: 35_000 }));
    let mut c = C::new(server()).unwrap();
    assert_eq!(c.raw(&[0, 0, 0, 13]), Err(Refusal::Malformed("a packet_length that is short or not a multiple of the block")));
    let mut c = C::new(server()).unwrap();
    let mut short_padding = plain_packet(&[20; 9]);
    short_padding[4] = 3;
    assert_eq!(c.raw(&short_padding), Err(Refusal::Malformed("padding under four bytes")));
    let mut c = C::new(server()).unwrap();
    let mut long_padding = plain_packet(&[20; 9]);
    long_padding[4] = 200;
    assert_eq!(c.raw(&long_padding), Err(Refusal::Malformed("padding longer than its packet")));
    // Before authentication a sealed packet past 35,000 bytes is refused, and
    // after it one up to 256 KiB is read.
    let mut c = with_service();
    let big = [&[2u8, 0, 0, 0x9c, 0x40][..], &[0; 40_000]].concat();
    assert!(matches!(c.send(&big), Err(Refusal::SealedLength { cap: 35_000, .. })));
    let mut c = logged_in();
    c.send(&big).unwrap();
    let huge = vec![2u8; 300 * 1024];
    assert!(matches!(c.send(&huge), Err(Refusal::SealedLength { cap: 262_144, .. })));
}

/// **A tampered packet ends the session without a word**: one flipped bit
/// anywhere in a sealed packet, length and tag included.
#[test]
fn a_tampered_sealed_packet_is_refused_in_silence() {
    for at in [0, 3, 4, 20, 33] {
        let mut c = handshake();
        let mut request = vec![5];
        string(&mut request, b"ssh-userauth");
        let mut packet = c.packet(&request);
        let at = at.min(packet.len() - 1);
        packet[at] ^= 0x10;
        let result = c.raw(&packet);
        assert!(matches!(result, Err(Refusal::Integrity | Refusal::SealedLength { .. })), "{result:?}");
        assert_eq!(c.server.output(), b"", "a refusal of a sealed packet says nothing");
    }
}

/// A channel open, its exec, and what the server sends back.
fn open(c: &mut C, peer: u32, window: u32, max: u32) -> u32 {
    let mut open = vec![90];
    string(&mut open, b"session");
    for v in [peer, window, max] {
        open.extend_from_slice(&v.to_be_bytes());
    }
    c.send(&open).unwrap();
    let reply = c.recv();
    let mut f = Fields(&reply[0]);
    assert_eq!((f.byte(), f.u32()), (91, peer));
    f.u32()
}

fn channel_request(ours: u32, name: &str, want_reply: bool, rest: &[u8]) -> Vec<u8> {
    let mut out = vec![98];
    out.extend_from_slice(&ours.to_be_bytes());
    string(&mut out, name.as_bytes());
    out.push(u8::from(want_reply));
    out.extend_from_slice(rest);
    out
}

fn exec(c: &mut C, ours: u32) -> toyos_ssh::ChannelId {
    let mut command = Vec::new();
    string(&mut command, b"true");
    c.send(&channel_request(ours, "exec", true, &command)).unwrap();
    let Some(Event::Exec { channel, command }) = c.events.pop() else { panic!("{:?}", c.events) };
    assert_eq!(command, b"true");
    channel
}

/// **Exec only, one per channel; ten channels; session channels only.**
#[test]
fn a_channel_serves_one_exec_and_nothing_else() {
    let mut c = logged_in();
    let ours = open(&mut c, 7, 1 << 20, 32 * 1024);
    for (name, rest) in [("shell", &[][..]), ("subsystem", &[0, 0, 0, 4, b's', b'f', b't', b'p'][..]), ("pty-req", &[][..]), ("env", &[][..])] {
        c.send(&channel_request(ours, name, true, rest)).unwrap();
        assert_eq!(c.recv(), [[&[100u8][..], &7u32.to_be_bytes()].concat()]);
        assert_eq!(c.events.pop(), Some(Event::Declined(Declined::Request(name.into()))));
    }
    exec(&mut c, ours);
    assert_eq!(c.recv(), [[&[99u8][..], &7u32.to_be_bytes()].concat()]);
    let mut again = Vec::new();
    string(&mut again, b"true");
    c.send(&channel_request(ours, "exec", true, &again)).unwrap();
    assert_eq!(c.recv(), [[&[100u8][..], &7u32.to_be_bytes()].concat()]);
    assert_eq!(c.events.pop(), Some(Event::Declined(Declined::Request("exec".into()))));
    // Up to ten channels, then a refusal by name.
    for peer in 1..10 {
        open(&mut c, 100 + peer, 1 << 20, 32 * 1024);
    }
    let mut eleventh = vec![90];
    string(&mut eleventh, b"session");
    eleventh.extend_from_slice(&[0, 0, 0, 99, 0, 1, 0, 0, 0, 0, 0x80, 0]);
    c.send(&eleventh).unwrap();
    let reply = c.recv();
    let mut f = Fields(&reply[0]);
    assert_eq!((f.byte(), f.u32(), f.u32()), (92, 99, 4));
    assert_eq!(c.events.pop(), Some(Event::Declined(Declined::ChannelLimit)));
    let mut global = vec![80];
    string(&mut global, b"no-more-sessions@openssh.com");
    global.push(1);
    c.send(&global).unwrap();
    assert_eq!(c.recv(), [vec![82]]);
    assert_eq!(c.events.pop(), Some(Event::Declined(Declined::GlobalRequest("no-more-sessions@openssh.com".into()))));
}

/// **A message naming a channel the session does not hold ends it**, and so
/// does data past the window or after EOF, and a window past 2^32 - 1.
#[test]
fn a_channel_message_is_held_to_its_channel() {
    let unknown = |c: &mut C| {
        let mut data = vec![94, 0, 0, 0, 5];
        string(&mut data, b"x");
        c.send(&data)
    };
    let mut c = logged_in();
    assert_eq!(unknown(&mut c), Err(Refusal::Malformed("a channel this session does not hold")));

    let mut c = logged_in();
    let ours = open(&mut c, 7, 1 << 20, 32 * 1024);
    let data = |len: usize| {
        let mut out = vec![94];
        out.extend_from_slice(&ours.to_be_bytes());
        string(&mut out, &vec![b'x'; len]);
        out
    };
    // The window is 2 MiB, in packets of at most 32 KiB.
    assert!(matches!(c.send(&data(32 * 1024 + 1)), Err(Refusal::TooLong { field: "data", .. })));
    let mut c = logged_in();
    let ours = open(&mut c, 7, 1 << 20, 32 * 1024);
    assert_eq!(ours, 0);
    for _ in 0..64 {
        c.send(&data(32 * 1024)).unwrap();
    }
    assert_eq!(c.send(&data(1)), Err(Refusal::Malformed("data beyond the window")));

    let mut c = logged_in();
    open(&mut c, 7, 1 << 20, 32 * 1024);
    c.send(&[96, 0, 0, 0, 0]).unwrap();
    assert!(matches!(c.events.pop(), Some(Event::Eof { .. })));
    assert_eq!(c.send(&data(1)), Err(Refusal::Malformed("data after the client's EOF")));

    let mut c = logged_in();
    open(&mut c, 7, u32::MAX - 1, 32 * 1024);
    let mut adjust = vec![93, 0, 0, 0, 0];
    adjust.extend_from_slice(&2u32.to_be_bytes());
    assert_eq!(c.send(&adjust), Err(Refusal::Malformed("a window past 2^32 - 1 bytes")));
}

/// **Flow control both ways.** The server sends no more than the client's
/// window, in packets no longer than its maximum, and says when it may send
/// more; the client's window comes back only as the driver consumes.
#[test]
fn the_windows_bound_both_sides() {
    let mut c = logged_in();
    let ours = open(&mut c, 7, 100, 40);
    let channel = exec(&mut c, ours);
    c.recv();
    assert_eq!(c.server.send(channel, &[b'a'; 150]), Ok(100));
    let sent = c.recv();
    assert_eq!(sent.iter().map(|p| Fields(&p[5..]).string().len()).collect::<Vec<_>>(), [40, 40, 20]);
    assert_eq!(c.server.send(channel, b"more"), Ok(0));
    let mut adjust = vec![93];
    adjust.extend_from_slice(&ours.to_be_bytes());
    adjust.extend_from_slice(&3u32.to_be_bytes());
    c.send(&adjust).unwrap();
    assert_eq!(c.events.pop(), Some(Event::Writable { channel }));
    assert_eq!(c.server.send_stderr(channel, b"more"), Ok(3));
    let stderr = c.recv();
    let mut f = Fields(&stderr[0]);
    assert_eq!((f.byte(), f.u32(), f.u32(), f.string()), (95, 7, 1, &b"mor"[..]));

    // 1 MiB delivered and consumed gives the client its window back; not
    // before.
    let mut data = vec![94];
    data.extend_from_slice(&ours.to_be_bytes());
    string(&mut data, &[b'x'; 32 * 1024]);
    for _ in 0..32 {
        c.send(&data).unwrap();
    }
    c.events.clear();
    c.server.consumed(channel, 32 * 32 * 1024 - 1).unwrap();
    assert_eq!(c.recv(), Vec::<Vec<u8>>::new());
    c.server.consumed(channel, 1).unwrap();
    let mut want = vec![93, 0, 0, 0, 7];
    want.extend_from_slice(&(1u32 << 20).to_be_bytes());
    assert_eq!(c.recv(), [want]);
}

/// **A closed channel's id names nothing again**: the client's CLOSE frees
/// the channel's place, and the channel opened in it is another, so an id the
/// driver still holds reaches no program but its own, and the client cannot
/// name the old number either. The driver's exit closes a channel from this
/// side, and a request on it after that tells the driver nothing.
#[test]
fn a_closed_channel_is_gone() {
    let mut c = logged_in();
    let old_number = open(&mut c, 7, 1 << 20, 32 * 1024);
    let old = exec(&mut c, old_number);
    c.recv();
    let mut data = vec![94];
    data.extend_from_slice(&old_number.to_be_bytes());
    string(&mut data, b"x");
    c.send(&data).unwrap();
    c.send(&[[97u8].as_slice(), &old_number.to_be_bytes()].concat()).unwrap();
    assert_eq!(c.recv(), [vec![97, 0, 0, 0, 7]]);
    assert_eq!(c.events, [Event::Data { channel: old, data: b"x".to_vec() }, Event::Closed { channel: old }]);
    c.events.clear();

    let number = open(&mut c, 8, 1 << 20, 32 * 1024);
    let channel = exec(&mut c, number);
    c.recv();
    assert_ne!((number, channel), (old_number, old));
    assert_eq!(c.server.send(old, b"x"), Err(Gone));
    assert_eq!(c.server.send_stderr(old, b"x"), Err(Gone));
    assert_eq!(c.server.consumed(old, 1), Err(Gone));
    assert_eq!(c.server.exit(old, 3), Err(Gone));
    assert_eq!(c.recv(), Vec::<Vec<u8>>::new(), "nothing reached the channel opened in the old one's place");
    assert_eq!(c.server.send(channel, b"y"), Ok(1));
    c.recv();

    c.server.exit(channel, 3).unwrap();
    let sent = c.recv();
    assert_eq!(sent.iter().map(|p| p[0]).collect::<Vec<_>>(), [98, 96, 97]);
    assert_eq!(c.server.exit(channel, 3), Err(Gone));
    c.send(&[[97u8].as_slice(), &number.to_be_bytes()].concat()).unwrap();
    assert_eq!(c.recv(), Vec::<Vec<u8>>::new(), "the client's CLOSE answers the server's");
    assert_eq!(c.events, []);
    assert_eq!(c.send(&data), Err(Refusal::Malformed("a channel this session does not hold")));

    // A channel the driver ended before its exec runs nothing.
    let mut c = logged_in();
    let number = open(&mut c, 9, 1 << 20, 32 * 1024);
    let mut data = vec![94];
    data.extend_from_slice(&number.to_be_bytes());
    string(&mut data, b"x");
    c.send(&data).unwrap();
    let Some(Event::Data { channel, .. }) = c.events.pop() else { panic!("{:?}", c.events) };
    c.server.exit(channel, 127).unwrap();
    c.recv();
    let mut command = Vec::new();
    string(&mut command, b"true");
    c.send(&channel_request(number, "exec", true, &command)).unwrap();
    c.send(&channel_request(number, "env", true, &[])).unwrap();
    assert_eq!(c.recv(), Vec::<Vec<u8>>::new());
    assert_eq!(c.events, [], "a request on a channel the driver ended is not the driver's");
}

/// **A re-exchange holds the session's output**: what the driver sends after
/// the client's KEXINIT goes out only after the server's NEWKEYS, sealed under
/// the new keys, and the session identifier stays the first exchange's.
#[test]
fn a_re_exchange_holds_output_until_its_newkeys() {
    let mut c = logged_in();
    let ours = open(&mut c, 7, 1 << 20, 32 * 1024);
    let channel = exec(&mut c, ours);
    c.recv();
    let id = c.session_id();
    let exchange = c.start_exchange("curve25519-sha256").unwrap();
    assert_eq!(c.server.send(channel, b"held"), Ok(4));
    let mut ignore = vec![2];
    string(&mut ignore, b"");
    c.send(&ignore).unwrap();
    c.finish_exchange(exchange).unwrap();
    assert_eq!(c.session_id(), id);
    let mut want = vec![94, 0, 0, 0, 7];
    string(&mut want, b"held");
    assert_eq!(c.recv(), [want]);
    // A session message in the middle of a re-exchange is refused.
    let exchange = c.start_exchange("curve25519-sha256").unwrap();
    let _ = exchange;
    assert_eq!(c.send(&[97, 0, 0, 0, 0]), Err(Refusal::Unexpected { phase: "a key re-exchange", message: 97 }));
}

/// The key the authorizer named is asked about again with the signature, so
/// one removed between the offer and the signature is refused.
#[test]
fn the_authorizer_is_asked_again_with_the_signature() {
    let (key, public) = pair(1);
    let keys = Keys { answers: 1, ..Keys::new(vec![public]) };
    let mut c = C::handshake(Server::new(host_key(), keys, SystemRandom::new())).unwrap();
    c.recv();
    c.service().unwrap();
    c.recv();
    let blob = ed25519_blob(&public);
    c.send(&probe("ssh-ed25519", &blob)).unwrap();
    assert_eq!(c.recv()[0][0], 60, "PK_OK while the key is named");
    c.sign_in(USER, &key).unwrap();
    assert_eq!(c.recv(), [failure()]);
    assert!(matches!(&c.events[..], [Event::Declined(Declined::Key { .. })]));
    assert!(!authenticated(&c));
}
