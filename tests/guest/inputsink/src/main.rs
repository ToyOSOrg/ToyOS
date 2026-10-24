//! Test-only: an owner of input with no screen. Every key a source sends on
//! `input` is relayed to the one test connected on `keys`, which asks first,
//! so the relay is a send on a connection that has carried a frame each way
//! and needs no memory of its own.
//!
//! `keys` frames: the test's [`HELLO`] is answered [`HELLO`]; then each key is
//! a [`KEY`] carrying the source's usage and whether it was released.

use toyos::endow;
use toyos::ipc::{Connection, FrameRx, RxStep};
use toyos::poller::{Poller, READABLE};
use toyos_input::wire::{self, Record, MAX_PAYLOAD};

/// The port the test watches keys on.
const KEYS: &str = "keys";
/// The test's first frame, and its answer.
const HELLO: u32 = 1;
/// A key: its usage, then 1 if it was released and 0 if pressed.
const KEY: u32 = 2;

const TOKEN_INPUT: u64 = 0;
const TOKEN_KEYS: u64 = 1;
const TOKEN_SOURCE: u64 = 2;
const TOKEN_WATCHER: u64 = 3;

fn main() {
    let inputs = endow::acceptor(wire::PORT).expect("inputsink: the manifest declares it serves `input`");
    let keys = endow::acceptor(KEYS).expect("inputsink: the manifest declares it serves `keys`");
    let mut source: Option<(Connection, FrameRx<MAX_PAYLOAD>)> = None;
    let mut watcher: Option<(Connection, FrameRx<0>)> = None;
    let poller = Poller::new(4);
    println!("inputsink: serving input");
    loop {
        poller.watch(&inputs, READABLE, TOKEN_INPUT);
        poller.watch(&keys, READABLE, TOKEN_KEYS);
        if let Some((conn, _)) = &source {
            poller.watch(conn, READABLE, TOKEN_SOURCE);
        }
        if let Some((conn, _)) = &watcher {
            poller.watch(conn, READABLE, TOKEN_WATCHER);
        }
        let mut ready = [false; 4];
        poller.wait(1, u64::MAX, |token| ready[token as usize] = true);

        // One source and one watcher: a later one takes the place of the last.
        if ready[TOKEN_INPUT as usize] {
            if let Ok(conn) = inputs.accept() {
                source = Some((conn, FrameRx::new()));
            }
        }
        if ready[TOKEN_KEYS as usize] {
            if let Ok(conn) = keys.accept() {
                watcher = Some((conn, FrameRx::new()));
            }
        }
        if ready[TOKEN_WATCHER as usize] {
            if let Some((conn, rx)) = &mut watcher {
                loop {
                    match rx.pump(conn) {
                        RxStep::Frame { msg_type: HELLO, .. } => {
                            let _ = conn.try_signal(HELLO);
                        }
                        RxStep::Frame { .. } | RxStep::Idle => break,
                        RxStep::Eof | RxStep::Malformed => {
                            watcher = None;
                            break;
                        }
                    }
                }
            }
        }
        if ready[TOKEN_SOURCE as usize] {
            let Some((conn, rx)) = &mut source else { continue };
            loop {
                let record = match rx.pump(conn) {
                    RxStep::Idle => break,
                    RxStep::Eof | RxStep::Malformed => {
                        source = None;
                        break;
                    }
                    RxStep::Frame { msg_type, payload_len, .. } => wire::decode(msg_type, rx.payload(payload_len)),
                };
                if let Ok(Record::Key { key, .. }) = record {
                    let released = u8::from(key.modifiers & toyos_abi::input::MOD_RELEASED != 0);
                    println!("inputsink: key {:#04x} {}", key.keycode, if released == 1 { "released" } else { "pressed" });
                    if let Some((watching, _)) = &watcher {
                        let _ = watching.try_send_bytes(KEY, &[key.keycode, released]);
                    }
                }
            }
        }
    }
}
