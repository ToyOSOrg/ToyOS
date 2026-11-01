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

use std::io::{BufRead, Write};

use toyos::ipc::FrameRx;
use toyos::poller::{READABLE, wait_one};
use toyos_wifi::{Answer, Band, Network, Request, Ssid, State};

const USAGE: &str = "usage: wifi status | scan | connect <network> | disconnect";

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
            let mut line = Vec::new();
            if std::io::stdin().lock().read_until(b'\n', &mut line).is_err() {
                return (false, "standard input held no passphrase".to_string());
            }
            let typed = line.strip_suffix(b"\n").unwrap_or(&line);
            let typed = typed.strip_suffix(b"\r").unwrap_or(typed);
            let answer = ask(Request::Connect { ssid, passphrase: typed });
            line.iter_mut().for_each(|b| *b = 0);
            return answer;
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
    let (msg_type, mut payload) = request.encode();
    let sent = wlan.send_bytes(msg_type, &payload);
    payload.iter_mut().for_each(|b| *b = 0);
    if let Err(e) = sent {
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
