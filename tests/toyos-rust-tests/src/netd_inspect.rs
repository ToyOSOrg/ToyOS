//! netd's own `inspect` answer, as the netd guest tests read it: one request,
//! answered within a bound said by name, and a count off it by its path.
//!
//! Each netd test that asks includes this file whole.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use toyos::ipc::{FrameRx, RxStep};
use toyos::poller::{Poller, READABLE};
use toyos_inspect::{Value, MAX_SNAPSHOT_BYTES, MSG_INSPECT, MSG_SNAPSHOT};

/// What netd answered: every path it reports, and its value.
pub struct Net(BTreeMap<String, Value>);

impl Net {
    /// Ask netd, and panic by name if it has not answered within `within`.
    pub fn ask(within: Duration) -> Self {
        let conn = toyos::endow::service("netd").expect("a connection to netd");
        conn.signal(MSG_INSPECT).expect("netd takes an inspect request");
        let poller = Poller::new(1);
        let mut rx: Box<FrameRx<MAX_SNAPSHOT_BYTES>> = Box::new(FrameRx::new());
        let deadline = Instant::now() + within;
        loop {
            match rx.pump(&conn) {
                RxStep::Frame { msg_type: MSG_SNAPSHOT, payload_len } => {
                    return Self(
                        toyos_inspect::decode(rx.payload(payload_len), toyos_inspect::NET)
                            .unwrap_or_else(|why| panic!("netd's snapshot: {why}")),
                    );
                }
                RxStep::Idle => {}
                other => panic!("netd answered inspect with {other:?}, not a snapshot"),
            }
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "netd did not answer inspect within {within:?}");
            poller.watch(&conn, READABLE, 0);
            poller.wait(1, left.as_nanos() as u64, |_| {});
        }
    }

    /// The count netd answers at `path`.
    pub fn count(&self, path: &str) -> u64 {
        match self.0.get(path) {
            Some(Value::U64(n)) => *n,
            other => panic!("netd's snapshot has {path} as {other:?}"),
        }
    }
}
