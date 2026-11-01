//! `/system/bin/wifi`: the machine's Wi-Fi, asked of `wlan` over `wifi`, the port only this
//! row holds.
//!
//! - `wifi status`: the radio, and the network joined or being joined.
//! - `wifi scan`: every network heard, strongest first, one line each.
//! - `wifi connect <network>`: join it with the passphrase on standard input's first line,
//!   which no argument list or log shows.
//! - `wifi disconnect`: leave it.
//!
//! The answer is printed on standard output, with exit status 0 where it was done and 1 where
//! it was refused.

use std::io::Write;

use toyos_crypto::Secret;

use toyos::ipc::FrameRx;
use toyos::poller::{READABLE, wait_one};
use toyos_wifi::{Answer, Band, Network, Request, Ssid, State};

const USAGE: &str = "usage: wifi status | scan | connect <network> | disconnect";

/// The most of standard input's first line kept: past the longest passphrase, `wlan` refuses
/// it whole.
const LINE: usize = toyos_wifi::MAX_PASSPHRASE + 1;

/// Standard input's first line into `line`: its length, or `None` where input ends before any
/// of it. A line longer than `line` is cut.
///
/// **Read from the descriptor itself**, never through std's `Stdin`, which on a terminal
/// echoes the passphrase as it is typed and keeps it in a line buffer: a workaround for
/// `issues/toyos-std-echoes-stdin-and-keeps-each-line.md`, deleted when it closes. A
/// backspace erases the octet before it, and other control octets are dropped.
fn passphrase(line: &mut [u8; LINE]) -> Option<usize> {
    let (mut len, mut byte) = (0usize, [0u8; 1]);
    loop {
        match toyos::files::read(0, &mut byte) {
            Ok(1) => match byte[0] {
                b'\n' | b'\r' => break,
                0x08 | 0x7f => len = len.saturating_sub(1).min(LINE),
                b if b < 0x20 => {}
                b => {
                    if let Some(at) = line.get_mut(len) {
                        *at = b;
                    }
                    len += 1;
                }
            },
            Ok(_) if len > 0 => break,
            _ => return None,
        }
    }
    Some(len.min(LINE))
}

/// How long an answer may take: longer than every bound `wlan` keeps on a join together.
const ANSWER_BOUND_NANOS: u64 = 60_000_000_000;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (done, text) = run(&args);
    println!("{text}");
    std::io::stdout().flush().expect("the answer reaches standard output");
    std::process::exit(if done { 0 } else { 1 });
}

fn run(args: &[String]) -> (bool, String) {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let request = match args[..] {
        ["status"] => Request::Status,
        ["scan"] => Request::Scan,
        ["disconnect"] => Request::Disconnect,
        ["connect", name] => {
            let Some(ssid) = Ssid::new(name.as_bytes()) else {
                return (false, "a network's name is 1 to 32 octets".to_string());
            };
            let mut line = Secret::new([0u8; LINE]);
            let Some(len) = passphrase(line.expose_mut()) else {
                return (false, "standard input held no passphrase line".to_string());
            };
            return ask(Request::Connect { ssid, passphrase: &line.expose()[..len] });
        }
        _ => return (false, USAGE.to_string()),
    };
    ask(request)
}

fn ask(request: Request<'_>) -> (bool, String) {
    let wlan = match toyos::endow::service(toyos_wifi::PORT) {
        Ok(conn) => conn,
        Err(e) => return (false, format!("wlan's wifi port did not answer: {e:?}")),
    };
    let mut payload = Secret::new([0u8; toyos_wifi::REQUEST_PAYLOAD]);
    let (msg_type, len) = request.encode(payload.expose_mut());
    if let Err(e) = wlan.send_bytes(msg_type, &payload.expose()[..len]) {
        return (false, format!("wlan did not take the request: {e:?}"));
    }
    match wait_one(&wlan, READABLE, ANSWER_BOUND_NANOS) {
        Ok(true) => {}
        Ok(false) => return (false, "wlan did not answer in time".to_string()),
        Err(e) => return (false, format!("wlan's connection failed: {e:?}")),
    }
    let mut rx = Box::new(FrameRx::<{ toyos_wifi::MAX_PAYLOAD }>::new());
    let frame = match rx.recv(&wlan) {
        Ok(frame) => frame,
        Err(e) => return (false, format!("wlan did not answer: {e:?}")),
    };
    match Answer::decode(frame.msg_type, rx.payload(frame.payload_len)) {
        Ok(Answer::Refused { refusal, detail }) if detail.is_empty() => (false, refusal.reason().to_string()),
        Ok(Answer::Refused { refusal, detail }) => (false, format!("{}: {detail}", refusal.reason())),
        Ok(Answer::Done) => (true, "disconnected".to_string()),
        Ok(Answer::Connected { band, channel, signal }) => {
            (true, format!("connected on {} channel {channel} at {} dBm", band_name(band), signal.dbm()))
        }
        Ok(Answer::State(state)) => (true, status(state)),
        Ok(Answer::Networks(networks)) => (true, networks.iter().map(line).collect::<Vec<_>>().join("\n")),
        Err(_) => (false, "wlan answered with a frame that does not decode".to_string()),
    }
}

fn band_name(band: Band) -> &'static str {
    match band {
        Band::TwoPointFour => "2.4 GHz",
        Band::Five => "5 GHz",
        Band::Six => "6 GHz",
    }
}

/// A network's name, its printable ASCII as it is and every other octet as `\xNN`.
fn name(ssid: &Ssid) -> String {
    ssid.as_bytes()
        .iter()
        .map(|&b| if (0x20..0x7f).contains(&b) && b != b'\\' { char::from(b).to_string() } else { format!("\\x{b:02x}") })
        .collect()
}

fn line(n: &Network) -> String {
    let joinable = if n.joinable { "" } else { " (not joinable)" };
    format!("{:>4} dBm  {:<7}  {:<18}  {}{joinable}", n.signal.dbm(), band_name(n.band), n.security.name(), name(&n.ssid))
}

fn status(state: State) -> String {
    match state {
        State::NoRadio => "no radio".to_string(),
        State::Idle => "not connected".to_string(),
        State::Scanning => "scanning".to_string(),
        State::Joining { ssid } => format!("joining {}", name(&ssid)),
        State::Connected { ssid, band, channel, signal } => {
            format!("connected to {} on {} channel {channel} at {} dBm", name(&ssid), band_name(band), signal.dbm())
        }
    }
}
