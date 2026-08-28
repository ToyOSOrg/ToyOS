//! The write-back queue: dirty-page teardown for a closed file, deferred
//! off the closing thread onto `iod`.
//!
//! A queued file stays pinned (`teardown_owed`) until drained; eviction
//! never takes a dirty page. Entries pop one at a time under the VFS lock.
//! A flush refused on budget re-enqueues rather than tearing down — no
//! flush is lost to a timeout. [`drain_held`] cannot park: a refusal there
//! re-enqueues and leaves the retry to `iod`.

use alloc::collections::VecDeque;
use alloc::string::String;

use toyos_abi::syscall::SyscallError;

use crate::completion::{self, Armed, Outcome, Subject, Token, Watch};
use crate::file_cache::{self, FileId, Teardown};
use crate::scheduler::Parkable;
use crate::sync::Lock;
use crate::time::Deadline;

struct Pending {
    file_id: FileId,
    path: String,
    mtime: u64,
}

static QUEUE: Lock<VecDeque<Pending>> = Lock::new(VecDeque::new());

/// The subject `iod` parks on: closes post here, and `iod` holds the sole arm across its loop.
pub static WORK: Watch = Watch::new();

/// Enqueues a dropped file's teardown and wakes `iod`; takes only the queue lock, so callable from `Drop`.
pub fn enqueue(file_id: FileId, path: String, mtime: u64) {
    QUEUE.lock().push_back(Pending { file_id, path, mtime });
    // Edge-triggered: `iod` rechecks the queue itself, never reads state off the post.
    completion::post(Subject::of(&WORK), Outcome::Ready);
}

/// The token `iod` arms with; opaque, but must match the post's arm token for the drop-time check.
pub const TOKEN: Token = Token::new(0);

/// Runs every pending teardown to durability; for callers holding no completion arm (`iod` uses [`drain_all_iod`]).
pub fn drain_all() {
    drain_retrying(crate::block::between_attempts);
}

/// Same as [`drain_all`], for `iod`: reuses its standing [`WORK`] arm since a second arm panics the machine.
pub fn drain_all_iod(parkable: &Parkable, armed: &Armed<'_>) {
    drain_retrying(|attempt| {
        if attempt <= 1 {
            crate::scheduler::yield_now();
        } else {
            // Reuses `armed`; arming again panics.
            let deadline = Deadline::at(crate::clock::now() + crate::block::backoff_step(attempt));
            let _ = completion::wait(parkable, armed, deadline);
        }
    });
}

/// Shared retry loop for [`drain_all`] and [`drain_all_iod`]: passes over the queue, backing off between passes, until drained or `block::DEADMAN` gives up.
fn drain_retrying(mut backoff: impl FnMut(u32)) {
    let deadman = Deadline::at(crate::clock::now() + crate::block::DEADMAN.duration());
    let mut attempt = 0u32;
    loop {
        let mut owed = false;
        // Snapshot: a re-enqueued entry waits for the next pass, not this one.
        let n = QUEUE.lock().len();
        for _ in 0..n {
            // Fresh VFS lock per entry: backoff below holds no lock.
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

/// Drains the queue under the caller's held VFS lock (`Vfs::open_backing`) so no device view is taken while an entry is owed; cannot park, so a refusal re-enqueues for [`drain_all`] to retry.
pub fn drain_held(vfs: &mut crate::vfs::Vfs) {
    // Bounds the pass: a re-enqueued entry is not spun on within this call.
    let n = QUEUE.lock().len();
    for _ in 0..n {
        if let Drained::Empty = drain_one(vfs, Deadline::never()) {
            break;
        }
    }
}

enum Drained {
    Empty,
    /// Torn down, including a give-up that left pages unflushed.
    Done,
    /// Never torn down: still pinned and owed, for a caller's retry loop to re-drive.
    Owed,
}

/// Pops one entry and flushes/tears it down under the caller's VFS lock; a budget refusal within `deadman` returns [`Drained::Owed`] instead.
fn drain_one(vfs: &mut crate::vfs::Vfs, deadman: Deadline) -> Drained {
    let Some(pending) = QUEUE.lock().pop_front() else {
        return Drained::Empty;
    };

    // A deleted file has nothing to flush; its handle is already torn down.
    let probe = file_cache::writeback_probe(pending.file_id);
    if probe.dirty_meta && !probe.deleted {
        // Tags the flush as the drain's, for `fat-mirror-write-refuse` to stage on this path.
        #[cfg(feature = "boot-actuators")]
        crate::fat32_adapter::enter_drain_flush();
        let flushed = vfs.flush_file(&pending.path, pending.file_id, pending.mtime);
        #[cfg(feature = "boot-actuators")]
        crate::fat32_adapter::leave_drain_flush();
        match flushed {
            Ok(()) => {}
            // Budget refusal, not a device fact: `flush_file` left `dirty_meta` set, so the re-enqueued entry redelivers the same bytes.
            Err(SyscallError::WouldBlock) if !deadman.reached(crate::clock::now()) => {
                // Re-enqueued under the same held VFS lock the pop used: never absent from both at once.
                QUEUE.lock().push_back(pending);
                return Drained::Owed;
            }
            // Device error or deadman: undurable; logged loudly since no caller can see this error.
            Err(e) => {
                crate::log!(
                    "writeback: {} is not durable ({e:?}); its unflushed pages are lost",
                    pending.path,
                );
            }
        }
    }

    // Re-checked under the VFS lock a re-open needs: either re-open adopted it or this removes the name.
    match file_cache::finish_writeback(pending.file_id) {
        Teardown::Released => vfs.close_file(&pending.path, pending.file_id),
        Teardown::Adopted | Teardown::Vanished => {}
    }
    Drained::Done
}
