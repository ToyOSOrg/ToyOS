//! The write-back queue: the flush a closed file's dirty pages owe, deferred
//! off the closing thread onto `iod`.
//!
//! **Why it exists.** `object::file::OpenFileState::drop` used to take the VFS
//! lock and flush the file to its device — a device round trip inside a `Drop`,
//! on whichever thread happened to close the last handle. That blocks the
//! closer on the disk, and once `vfs::VFS` becomes a sleep lock a `Drop` cannot
//! take it at all: a `Drop` has no [`crate::scheduler::Parkable`]. So the flush
//! leaves the closing thread. It leaves it here — the last close pins the file
//! (`file_cache`'s `teardown_owed`), pushes it on this queue and returns; `iod`
//! (and `SYS_SHUTDOWN`) drain the queue and run the teardown the `Drop` used to,
//! under the VFS lock. This chunk converts no lock: the VFS is still a spinlock
//! and `iod` spins in the driver like any thread. It is the prerequisite the
//! owner ruled (wall 4) for the conversion, recorded in
//! `issues/kernel/every-wait-in-this-kernel-is-a-spin.md`.
//!
//! **The pin is the whole correctness argument.** A file on this queue is kept
//! alive by `teardown_owed` even at `ref_count == 0`, and eviction never takes a
//! dirty page — so the dirty pages a closed file owes outlive the handle that
//! dirtied them, and a re-open before the drain finds them in the cache rather
//! than reading a device that has not been written yet
//! (`file_cache::release_to_writeback`).
//!
//! **The drain pops under the VFS lock, and that is a durability requirement,
//! not a nicety.** `SYS_SHUTDOWN` drains the queue and then calls
//! `Vfs::sync_all` to commit the devices' own write caches. If the drain popped
//! an entry and released every lock before flushing it, `sync_all` could run in
//! that gap and commit a device that had not yet received the file's bytes. So
//! each entry is popped from the queue while the VFS lock is held: a pending
//! file is always either still on the queue or being flushed under that lock,
//! never in a gap `sync_all` can pass through.
//!
//! **The pin covers the file cache and nothing else, so a reader that goes
//! round the cache has to settle the queue itself.** `Vfs::open_backing` hands
//! out a view of the *device* — the extent list and the length a filesystem has
//! recorded — and the whole point of this queue is that the device is not
//! current while an entry is on it. A `/home` file written, closed and then
//! spawned answered `ELF: fewer bytes than a file header`, because
//! `loader::spawn` read a btree inode that still said length 0. So
//! [`drain_held`] runs the queue out from `Vfs::open_backing` before any backing
//! is derived, and the invariant it restores is stated once: **no device view is
//! taken while this queue owes that device anything.** All of it and not the one
//! path, because the queue is keyed by the path a handle was opened under and a
//! symlink, a rename or a `cwd`-relative open name the same file differently —
//! a matcher would answer "nothing owed" for exactly the cases nobody would
//! test. What it costs the reader is the backlog: `iod`'s header records ~200 µs
//! a file, measured, and it is the same work in either thread.
//!
//! **A budget refusal is not durable yet, and not a loss.** A flush can be
//! refused on the block layer's operation budget (`block::OPERATION`) — a
//! starved host descheduling the vCPU past two seconds mid-flush, never a device
//! fact — and `SYS_CLOSE` did not wait, so this deferred flush is the only thing
//! standing behind the pages the close promised. So a `WouldBlock` re-enqueues
//! the file rather than tearing it down ([`Drained::Owed`]), and [`drain_all`]
//! retries it on a fresh budget off the pinned path, bounded by `block::DEADMAN`,
//! exactly as `SYS_FSYNC`'s loop does (`object/ops.rs`). The invariant is
//! `SYS_FSYNC`'s: **a timed-out flush discards nothing.** The pin (not queue
//! membership) is what guarantees it — a re-enqueue drops the entry from the
//! queue for an instant, but the refused flush issued nothing to the device and
//! the file stays pinned and dirty, so no `sync_all` in that instant can call it
//! durable and no retry can lose it. Only the device's own word (`Io`) or the
//! deadman ends the retries, and either tears the file down with a loud line,
//! because at that point nothing can make it durable and a deferred flush has no
//! caller to return the error to.
//!
//! **[`drain_held`] cannot park, so it does not retry — it re-enqueues and
//! leaves the retry to `iod`.** Its caller holds the VFS spinlock across the
//! backlog, and a backoff there would park a lock-holder (§6.4). So a refusal on
//! that path keeps the file owed for `drain_all` to re-drive off the lock; the
//! spawn that drove it may read a device still a beat behind for that one file
//! and get the loud short read `loader::spawn` reports, which a retry resolves —
//! never the silent stale read the pin exists to prevent, and never a lost page.

use alloc::collections::VecDeque;
use alloc::string::String;

use toyos_abi::syscall::SyscallError;

use crate::completion::{self, Armed, Outcome, Subject, Token, Watch};
use crate::file_cache::{self, FileId, Teardown};
use crate::scheduler::Parkable;
use crate::sync::Lock;
use crate::time::Deadline;

/// One file owing a write-back teardown: its cache id, the path the flush
/// resolves through, and the mtime to stamp.
struct Pending {
    file_id: FileId,
    path: String,
    mtime: u64,
}

static QUEUE: Lock<VecDeque<Pending>> = Lock::new(VecDeque::new());

/// The subject `iod` parks on. A closed file posts here; `iod` is the only
/// waiter, and it holds one arm across its whole loop so a post that lands while
/// it is draining still leaves a record its next wait returns on.
pub static WORK: Watch = Watch::new();

/// Push a file whose last handle just dropped, and wake `iod`.
///
/// **Context-free**: it takes only the queue's leaf lock and posts — no VFS
/// lock, no device — so it is callable from `OpenFileState::drop`, including
/// from `ops::close_all` under the `ProcessData` lock. `completion::post` takes
/// one leaf lock and stores, so it too is safe under a lock.
pub fn enqueue(file_id: FileId, path: String, mtime: u64) {
    QUEUE.lock().push_back(Pending { file_id, path, mtime });
    // Edge form: "state may have moved". The queue is the authoritative
    // predicate; the record only ensures `iod`'s next wait does not park.
    completion::post(Subject::of(&WORK), Outcome::Ready);
}

/// The token `iod` arms with. Opaque — nothing reads it — but a post carries
/// the waiter's own arm token, so it must match for the arm's drop-time
/// invariant check to hold.
pub const TOKEN: Token = Token::new(0);

/// Run every pending teardown now, making each file durable. Called by
/// `SYS_SHUTDOWN` before `Vfs::sync_all` — a caller that holds no completion arm,
/// so the inter-attempt backoff is `block::between_attempts` (which arms the
/// task's own watch). `iod` cannot use this — it holds a standing [`WORK`] arm —
/// and calls [`drain_all_iod`] instead.
///
/// The retry ladder and its `block::DEADMAN` bound are `SYS_FSYNC`'s; see the
/// module header for why a refused flush loses nothing.
pub fn drain_all() {
    drain_retrying(|attempt| crate::block::between_attempts(attempt));
}

/// The same, for `iod`, whose standing [`WORK`] arm forbids a second
/// (`completion::arm`'s one-arm-per-task rule). The backoff waits that arm out
/// for `block::backoff_step` rather than arming the task again — so a new close
/// posted to `WORK` wakes the retry early, and the machine no longer panics at
/// attempt >= 2, the depth `between_attempts` first parks at.
///
/// The first retry still only yields (no arm taken); the deadline arms come only
/// at attempt >= 2, and they come to the arm `iod` already holds. `parkable` and
/// `armed` are `iod`'s own, threaded down so the wait reuses them.
pub fn drain_all_iod(parkable: &Parkable, armed: &Armed<'_>) {
    drain_retrying(|attempt| {
        if attempt <= 1 {
            // The budget was usually spent by lock-wait or a descheduled vCPU,
            // over by the next slice; a yield takes no arm.
            crate::scheduler::yield_now();
        } else {
            // Wait on the arm `iod` already holds, for the same span
            // `between_attempts` would park — a `WORK` post (a new close) ends it
            // early and the next pass drains that file too.
            let deadline = Deadline::at(crate::clock::now() + crate::block::backoff_step(attempt));
            let _ = completion::wait(parkable, armed, deadline);
        }
    });
}

/// The retry loop shared by [`drain_all`] and [`drain_all_iod`]: pass over the
/// queue, and while a pass left a flush refused on budget, run `backoff` (off the
/// VFS lock, which every entry took and dropped) and pass again — until the queue
/// is drained or `block::DEADMAN` turns the last refusals into give-ups. The two
/// callers differ only in how `backoff` waits, because only one of them holds an
/// arm.
fn drain_retrying(mut backoff: impl FnMut(u32)) {
    let deadman = Deadline::at(crate::clock::now() + crate::block::DEADMAN.duration());
    let mut attempt = 0u32;
    loop {
        let mut owed = false;
        // Snapshot the length: an entry re-enqueued on a budget refusal waits
        // for the next pass, not this one.
        let n = QUEUE.lock().len();
        for _ in 0..n {
            // A fresh VFS lock per entry, dropped before the next — so a backlog
            // does not shut every other VFS caller out for its whole length, and
            // the backoff below holds no lock.
            match drain_one(&mut crate::vfs::lock(), deadman) {
                Drained::Empty => break,
                Drained::Done => {}
                Drained::Owed => owed = true,
            }
        }
        if !owed {
            return;
        }
        attempt += 1;
        backoff(attempt);
    }
}

/// Drain the queue for a caller that already holds the VFS across the backlog.
///
/// `Vfs::open_backing` is the caller and the module header says why: a backing
/// is a view of the device, and an entry on this queue is the statement that the
/// device is behind. The lock is held across the whole backlog here because the
/// caller's own hold is what makes its next read coherent — releasing it between
/// entries would let another close enqueue a file the caller is about to derive a
/// backing for.
///
/// **It cannot park, so it does not retry.** A budget refusal here re-enqueues
/// the file (with `Deadline::never()`, never a give-up) and leaves it for
/// [`drain_all`] to re-drive off the lock — backing off under the caller's
/// spinlock would park a lock-holder (§6.4). A snapshot bounds the pass so a
/// re-enqueued entry is not spun on. The module header says what that costs a
/// spawn whose own file was the one refused.
pub fn drain_held(vfs: &mut crate::vfs::Vfs) {
    let n = QUEUE.lock().len();
    for _ in 0..n {
        if let Drained::Empty = drain_one(vfs, Deadline::never()) {
            break;
        }
    }
}

/// What became of one entry.
enum Drained {
    /// The queue was empty.
    Empty,
    /// Flushed (or skipped) and torn down — or given up on a device error or the
    /// deadman and torn down, its unflushed pages lost.
    Done,
    /// The flush was refused on budget within the deadman: re-enqueued, still
    /// pinned and owed, **never torn down**, for a caller's retry loop to
    /// re-drive on a fresh budget.
    Owed,
}

/// One entry, popped and — under the VFS lock the caller holds — flushed and
/// torn down. The teardown the old `Drop` ran, relocated here: flush the dirty
/// pages, re-check the last reference, drop the file and release its filesystem
/// handle.
///
/// A flush refused on the operation budget (`WouldBlock`) within `deadman` is
/// [`Drained::Owed`] instead: the entry is re-enqueued and the file left pinned,
/// so the pages the close promised are not lost while a fresh budget is found.
fn drain_one(vfs: &mut crate::vfs::Vfs, deadman: Deadline) -> Drained {
    let Some(pending) = QUEUE.lock().pop_front() else {
        return Drained::Empty;
    };

    // (a) The flush, relocated from `OpenFileState::drop`. A deleted file
    // has nothing worth flushing — its data is going away and its
    // filesystem handle is already torn down — so it is skipped.
    let probe = file_cache::writeback_probe(pending.file_id);
    if probe.dirty_meta && !probe.deleted {
        // Mark the flush as the drain's, so `fat-mirror-write-refuse` stages its
        // budget expiry on this path and not on `SYS_FSYNC`. Folds to nothing
        // without `boot-actuators`.
        #[cfg(feature = "boot-actuators")]
        crate::fat32_adapter::enter_drain_flush();
        let flushed = vfs.flush_file(&pending.path, pending.file_id, pending.mtime);
        #[cfg(feature = "boot-actuators")]
        crate::fat32_adapter::leave_drain_flush();
        match flushed {
            Ok(()) => {}
            // Not durable *yet* — the caller's budget, not the device. Keep the
            // file owed and pinned (no `finish_writeback`) and hand it back for a
            // retry on a fresh budget, unless the deadman says the run of retries
            // is over. `flush_file` already restored the file's `dirty_meta` and
            // left every page dirty, so the re-enqueued entry re-delivers the same
            // bytes. The push-back is under the held VFS lock — the same
            // VFS-then-queue order every pop takes.
            Err(SyscallError::WouldBlock) if !deadman.reached(crate::clock::now()) => {
                QUEUE.lock().push_back(pending);
                return Drained::Owed;
            }
            // The device's own word, or the deadman: this cannot be made durable,
            // and the pages go with it. Loud, because a deferred flush has no
            // caller the error could reach otherwise.
            Err(e) => {
                crate::log!(
                    "writeback: {} is not durable ({e:?}); its unflushed pages are lost",
                    pending.path,
                );
            }
        }
    }

    // (b)/(c) The last-ref half of the old `release`, re-checked under the VFS
    // lock a re-open would need — so either the re-open won (the file is adopted
    // and left) or this drain won and removes the name too.
    match file_cache::finish_writeback(pending.file_id) {
        Teardown::Released => vfs.close_file(&pending.path, pending.file_id),
        Teardown::Adopted | Teardown::Vanished => {}
    }
    Drained::Done
}
