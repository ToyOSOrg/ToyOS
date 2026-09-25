//! `/system/bin/inspect [--json] [SELECTOR]` — what the machine's owners say
//! about themselves, now.
//!
//! The reader asks every owner in `toyos_inspect::OWNERS` whose root the
//! selector can reach, through the connector this process was given for that
//! owner's port, and prints the paths the selector matches as `path = value`
//! lines sorted by path, or as one JSON object. The grammar, the wire form and
//! the renderings are `toyos-inspect`'s; this file is the connections.
//!
//! **An owner this process holds no connector for is a refusal, not a gap**:
//! it is named on stderr and the run exits 2, so a pipe never mistakes a partial
//! answer for a whole one. The same for an owner whose port is closed, one that
//! does not answer within [`ANSWER_BOUND`], and one whose answer is not a
//! snapshot for its own root. Exit 1 is every owner answering and nothing
//! matching, as `grep` says it.

use std::collections::BTreeMap;
use std::io::Write;
use std::time::{Duration, Instant};

use toyos::endow::{self, EndowError, Endowments, SYSCAP_LABEL};
use toyos::syscap::SysCap;
use toyos_abi::inventory::{RawRecord, Record};
use toyos_abi::syscall::SyscallError;
use toyos::ipc::{self, FrameRx, RxStep};
use toyos::poller::{Poller, READABLE};
use toyos_inspect::{Invocation, Owner, Value, MAX_SNAPSHOT_BYTES, MSG_INSPECT, MSG_SNAPSHOT};

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

const USAGE: &str = "usage: inspect [--json] [SELECTOR]";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let run = match Invocation::parse(args.iter().map(String::as_str)) {
        Ok(run) => run,
        Err(why) => {
            eprintln!("inspect: {why}\n{USAGE}");
            std::process::exit(2);
        }
    };

    let mut found: BTreeMap<String, Value> = BTreeMap::new();
    let mut refused = false;
    for owner in toyos_inspect::OWNERS.into_iter().filter(|o| run.selector.reaches(o.root)) {
        match ask(owner) {
            Ok(snapshot) => {
                found.extend(snapshot.into_iter().filter(|(path, _)| run.selector.matches(path)))
            }
            Err(why) => {
                eprintln!("inspect: {}.*: {why}", owner.root);
                refused = true;
            }
        }
    }

    if run.selector.reaches(toyos_inspect::dev::ROOT) {
        match inventory() {
            Ok(records) => found.extend(
                toyos_inspect::dev::render(&records)
                    .into_iter()
                    .filter(|(path, _)| run.selector.matches(path)),
            ),
            Err(why) => {
                eprintln!("inspect: {}.*: {why}", toyos_inspect::dev::ROOT);
                refused = true;
            }
        }
    }

    let out = if run.json {
        toyos_inspect::json(found.iter().map(|(p, v)| (p.as_str(), v)))
    } else {
        found.iter().map(|(p, v)| toyos_inspect::line(p, v) + "\n").collect()
    };
    let mut stdout = std::io::stdout().lock();
    // A reader whose pipe closed early (`| head`) has nobody left to tell.
    let _ = stdout.write_all(out.as_bytes()).and_then(|()| stdout.flush());

    std::process::exit(if refused {
        2
    } else if found.is_empty() {
        1
    } else {
        0
    });
}

/// One owner's snapshot, or why there is none.
fn ask(owner: Owner) -> Result<BTreeMap<String, Value>, String> {
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

/// How many times the inventory is asked for again after the machine changed
/// between counting it and reading it. Policy: a device arriving on every
/// round is a machine the reader names rather than chases.
const INVENTORY_ROUNDS: usize = 4;

/// The kernel's inventory, asked with this process's `SysCap`.
fn inventory() -> Result<Vec<Record>, String> {
    let Some(cap) = Endowments::get().take::<SysCap>(SYSCAP_LABEL) else {
        return Err("this program holds no system capability, so the inventory is not its to read"
            .to_string());
    };
    let refused = |e: SyscallError| match e {
        SyscallError::PermissionDenied => {
            "the kernel refused: this program's capability does not carry `inventory`".to_string()
        }
        other => format!("the kernel refused the inventory ({other:?})"),
    };
    for _ in 0..INVENTORY_ROUNDS {
        let count = cap.inventory(&mut []).map_err(refused)?;
        let mut raw = vec![RawRecord::EMPTY; count];
        match cap.inventory(&mut raw) {
            Ok(n) => {
                return raw[..n]
                    .iter()
                    .map(|r| Record::decode(r).map_err(|why| format!("a record did not decode: {why}")))
                    .collect();
            }
            // The machine grew between the two calls.
            Err(SyscallError::ResourceExhausted) => continue,
            Err(e) => return Err(refused(e)),
        }
    }
    Err(format!("the machine changed on each of {INVENTORY_ROUNDS} reads of its inventory"))
}
