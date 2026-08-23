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
//! **A budget refusal is not durable yet, and not a loss.** A flush can be
//! refused on the block layer's operation budget (`block::OPERATION`) — a
//! starved host descheduling the vCPU past two seconds mid-flush, never a device
//! fact — and `SYS_CLOSE` did not wait, so this deferred flush is the only thing
//! standing behind the pages the close promised. So a `WouldBlock` re-enqueues
//! the file rather than tearing it down, and [`drain_all`] retries it on a fresh
//! budget off the pinned path, bounded by `block::DEADMAN`, exactly as
//! `SYS_FSYNC`'s loop does (`object/ops.rs`). The invariant is `SYS_FSYNC`'s:
//! **a timed-out flush discards nothing.** The pin (not queue membership) is
//! what guarantees it — a re-enqueue drops the entry from the queue for an
//! instant, but the refused flush issued nothing to the device and the file
//! stays pinned and dirty, so no `sync_all` in that instant can call it durable
//! and no retry can lose it. Only the device's own word (`Io`) or the deadman
//! ends the retries, and either tears the file down with a loud line, because at
//! that point nothing can make it durable and a deferred flush has no caller to
//! return the error to.

use alloc::collections::VecDeque;
use alloc::string::String;

use toyos_abi::syscall::SyscallError;

use crate::completion::{self, Outcome, Subject, Token, Watch};
use crate::file_cache::{self, FileId, Teardown};
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

/// Run every pending teardown now, making each file durable. Called by `iod` on
/// each wake and by `SYS_SHUTDOWN` before `Vfs::sync_all`.
///
/// One [`drain_pass`] pops, flushes and tears down every entry the queue holds
/// now, each under one VFS-lock hold; a flush refused on budget (`WouldBlock`)
/// re-enqueues the file, still owed, rather than tearing it down. When a pass
/// left anything owed this backs off on a fresh budget — off the VFS lock, which
/// every entry took and dropped — and passes again, until the queue is drained
/// or `block::DEADMAN` (checked per entry in [`drain_pass`]) turns the last
/// refusals into give-ups. The retry ladder is `SYS_FSYNC`'s
/// (`block::between_attempts`); see the module header for why a refused flush
/// loses nothing.
pub fn drain_all() {
    let deadman = Deadline::at(crate::clock::now() + crate::block::DEADMAN.duration());
    let mut attempt = 0u32;
    while drain_pass(deadman) {
        attempt += 1;
        crate::block::between_attempts(attempt);
    }
}

/// One pass over the entries the queue holds now. `true` when at least one was
/// re-enqueued on a budget refusal and the caller should back off and pass
/// again.
///
/// Each entry is popped **while the VFS lock is held** (see the module header),
/// the lock given up between entries. A snapshot of the length bounds the pass,
/// so an entry re-enqueued on `WouldBlock` is retried on the *next* pass and not
/// spun on within this one.
fn drain_pass(deadman: Deadline) -> bool {
    let mut owed = false;
    let n = QUEUE.lock().len();
    for _ in 0..n {
        let mut vfs = crate::vfs::lock();
        let Some(pending) = QUEUE.lock().pop_front() else { break };

        // (a) The flush, relocated from `OpenFileState::drop`. A deleted file
        // has nothing worth flushing — its data is going away and its
        // filesystem handle is already torn down — so it is skipped.
        let probe = file_cache::writeback_probe(pending.file_id);
        if probe.dirty_meta && !probe.deleted {
            // Mark the flush as the drain's, so `fat-mirror-write-refuse` stages
            // its budget expiry on this path and not on `SYS_FSYNC`. Folds to
            // nothing without `boot-actuators`.
            #[cfg(feature = "boot-actuators")]
            crate::fat32_adapter::enter_drain_flush();
            let flushed = vfs.flush_file(&pending.path, pending.file_id, pending.mtime);
            #[cfg(feature = "boot-actuators")]
            crate::fat32_adapter::leave_drain_flush();
            match flushed {
                Ok(()) => {}
                // Not durable *yet* — the caller's budget, not the device. Keep
                // the file owed and pinned (no `finish_writeback`) and retry on a
                // fresh budget, unless the deadman says the run of retries is
                // over. `flush_file` already restored the file's `dirty_meta` and
                // left every page dirty, so the re-enqueued entry re-delivers the
                // same bytes.
                Err(SyscallError::WouldBlock) if !deadman.reached(crate::clock::now()) => {
                    drop(vfs);
                    QUEUE.lock().push_back(pending);
                    owed = true;
                    continue;
                }
                // The device's own word, or the deadman: this cannot be made
                // durable, and the pages go with it. Loud, because a deferred
                // flush has no caller the error could reach otherwise.
                Err(e) => {
                    crate::log!(
                        "writeback: {} is not durable ({e:?}); its unflushed pages are lost",
                        pending.path,
                    );
                }
            }
        }

        // (b)/(c) The last-ref half of the old `release`, re-checked under the
        // VFS lock a re-open would need — so either the re-open won (the file is
        // adopted and left) or this drain won and removes the name too.
        match file_cache::finish_writeback(pending.file_id) {
            Teardown::Released => vfs.close_file(&pending.path, pending.file_id),
            Teardown::Adopted | Teardown::Vanished => {}
        }
    }
    owed
}
