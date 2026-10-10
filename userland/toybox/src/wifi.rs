//! YOGA WIFI HACK (measurement image only, never lands): `wifi scan [reset|bare]`
//! asks netstack for one scan of the AX200 and prints the networks it heard.

use std::time::{Duration, Instant};

use toyos::ipc::{FrameRx, RxStep, MAX_FRAME_LEN};
use toyos::poller::{Poller, READABLE};

/// netstack's `wifi::MSG_WIFI_SCAN`.
const MSG_WIFI_SCAN: u32 = u32::from_le_bytes(*b"wifi");
/// `toyos::net::RespType::Result` and `Error`.
const RESULT: u32 = 128;
const ERROR: u32 = 129;
/// A passive scan of every channel takes seconds; a bring-up before it more.
const ANSWER_BOUND: Duration = Duration::from_secs(40);

pub fn main(args: Vec<String>) {
    let mode = match (args.first().map(String::as_str), args.get(1).map(String::as_str)) {
        (Some("scan"), None) => 0u8,
        (Some("scan"), Some("reset")) => 1,
        (Some("scan"), Some("bare")) => 2,
        _ => {
            eprintln!("usage: wifi scan [reset|bare]");
            eprintln!("  reset: bring the AX200 up again from reset before scanning");
            eprintln!("  bare:  the same, without the PHY and MAC contexts");
            return;
        }
    };
    let conn = match toyos::endow::service("netstack") {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("wifi: no netstack to ask ({e:?})");
            return;
        }
    };
    if let Err(e) = conn.send_bytes(MSG_WIFI_SCAN, &[mode]) {
        eprintln!("wifi: netstack would not take the request ({e:?})");
        return;
    }
    println!("wifi: scanning...");
    let poller = Poller::new(1);
    let mut rx: Box<FrameRx<{ MAX_FRAME_LEN as usize }>> = Box::new(FrameRx::new());
    let deadline = Instant::now() + ANSWER_BOUND;
    loop {
        match rx.pump(&conn) {
            RxStep::Frame { msg_type: RESULT, payload_len } => {
                print!("{}", String::from_utf8_lossy(rx.payload(payload_len)));
                return;
            }
            RxStep::Frame { msg_type: ERROR, payload_len } => {
                eprintln!("wifi: netstack answered an error {:?}", rx.payload(payload_len));
                return;
            }
            RxStep::Frame { msg_type, .. } => {
                eprintln!("wifi: netstack answered message {msg_type:#x}");
                return;
            }
            RxStep::Eof => {
                eprintln!("wifi: netstack closed the connection without answering");
                return;
            }
            RxStep::Malformed => {
                eprintln!("wifi: netstack sent a frame this protocol cannot describe");
                return;
            }
            RxStep::Idle => {}
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            eprintln!("wifi: no answer in {} s", ANSWER_BOUND.as_secs());
            return;
        }
        poller.watch(&conn, READABLE, 0);
        poller.wait(1, left.as_nanos() as u64, |_| {});
    }
}
