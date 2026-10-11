//! The reader's one question: an owner's snapshot, asked through the connector
//! this process was given for that owner's port.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use toyos::endow::{self, EndowError};
use toyos::ipc::{self, FrameRx, RxStep};
use toyos::poller::{Poller, READABLE};
use toyos_inspect::{Owner, Value, MAX_SNAPSHOT_BYTES, MSG_INSPECT, MSG_SNAPSHOT};

/// How long one owner has to answer.
///
/// Policy, and generous: an owner answers on its next loop pass, and every
/// owner's pass is bounded by a frame or a period. What this bounds is an owner
/// that is wedged, which the reader names instead of joining it.
const ANSWER_BOUND: Duration = Duration::from_secs(2);

const _: () = assert!(
    MAX_SNAPSHOT_BYTES == ipc::MAX_FRAME_LEN as usize,
    "a snapshot is one frame, so its bound is the frame's"
);

/// One owner's snapshot, or why there is none.
pub fn ask(owner: Owner) -> Result<BTreeMap<String, Value>, String> {
    let conn = endow::service(owner.port).map_err(|e| match e {
        EndowError::NotEndowed => format!(
            "this program holds no `{}` connector, so {} is not its to read",
            owner.port, owner.root
        ),
        EndowError::ServerGone => format!("`{}` is not running: its port is closed", owner.port),
        EndowError::Refused(e) => format!("the kernel refused a connection to `{}` ({e:?})", owner.port),
    })?;
    conn.signal(MSG_INSPECT)
        .map_err(|e| format!("`{}` would not take the request ({e:?})", owner.port))?;

    let poller = Poller::new(1);
    let mut rx: Box<FrameRx<MAX_SNAPSHOT_BYTES>> = Box::new(FrameRx::new());
    let deadline = Instant::now() + ANSWER_BOUND;
    loop {
        match rx.pump(&conn) {
            RxStep::Frame { msg_type: MSG_SNAPSHOT, payload_len } => {
                return toyos_inspect::decode(rx.payload(payload_len), owner)
                    .map_err(|why| format!("`{}` answered something that is not a snapshot: {why}", owner.port));
            }
            RxStep::Frame { msg_type, .. } => {
                return Err(format!("`{}` answered message {msg_type:#x}, not a snapshot", owner.port));
            }
            RxStep::Eof => {
                return Err(format!(
                    "`{}` closed the connection without answering",
                    owner.port
                ));
            }
            RxStep::Malformed => {
                return Err(format!("`{}` sent a frame this protocol cannot describe", owner.port));
            }
            RxStep::Idle => {}
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(format!("`{}` did not answer within {ANSWER_BOUND:?}", owner.port));
        }
        poller.watch(&conn, READABLE, 0);
        poller.wait(1, left.as_nanos() as u64, |_| {});
    }
}
