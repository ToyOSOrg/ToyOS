//! What a refused batch does to this boot's log volume.
//!
//! **One pure function, because this is the whole refused-batch policy and it used to be
//! `if let Err(_) = … { volume = None }`.** Everything above [`fate`] is I/O
//! and everything below it is what the console says; the decision itself is a
//! function of three things a host test can hand it, which is why this file
//! exists rather than the two lines it replaces living in the loop.
//!
//! # The distinction this is built on
//!
//! `SYS_FSYNC` can fail for two reasons that used to arrive as one word. A
//! stick that cannot flush is `io::ErrorKind::Other` — `SyscallError::Io`,
//! which is what `kernel/src/block.rs`'s `BlockError::Device` becomes — and
//! ending the boot's log on one is right: the writes are not durable and no
//! number of retries will make them so. A *budget* that expired is
//! `io::ErrorKind::WouldBlock` — `BlockError::BudgetExpired` through
//! `toyos_fat32::Error::BudgetExpired` and `SyscallError::WouldBlock` — and
//! ending the log on one is wrong: nothing was issued, the device is untouched,
//! and the next operation gets a whole fresh `block::OPERATION`.
//!
//! The measurement that made the difference worth threading (2026-08-22): one
//! red in 73 full 12-wide suites, a guest that spent `syscall_wall=2108ms`
//! inside one `SYS_FSYNC` while its peers booted in 1,385 ms, and a boot's log
//! ended for a stick that answered every transfer.
//!
//! # Who bounds slowness now (owner ruling 2026-08-23)
//!
//! The kernel. `SYS_FSYNC` retries a budget-refused flush itself, between
//! attempts and off every lock, bounded by `kernel/src/block.rs`'s `DEADMAN` —
//! so a syscall this program makes either comes back durable inside that
//! deadman or comes back with a device error, and the three ways a volume dies
//! are all device facts by the time they arrive here: an error status, a reset
//! escalation that failed, or the deadman expired. What is left to this
//! program is the *policy over answers*: an error ends the volume, a refusal
//! that would block retries inside its own bounded run, and a round that
//! succeeded slowly is **degraded, not dead** — the records are durable, the
//! volume is kept, and the console says so once. While a slow round is in
//! flight nothing is published and nothing is dropped: the records it covers
//! are in the file, `LOG_DURABLE_NS` has not moved past them, and the reader
//! parks on the ring — buffered, not yet durable, said in one word.
//!
//! # Why only the flush retries
//!
//! A refused *append* is bytes this boot's file will not have — the records
//! have already left the cursor, so there is nothing to re-write — and a file
//! with a hole in it is exactly what this program's give-up policy exists to
//! stop pretending about. A refused *flush* loses nothing: the bytes are in the
//! file, and the next batch's flush covers them as well as its own. So the
//! asymmetry is deliberate and is what [`Step`] is for.

use std::time::Duration;

/// The longest a run of refused batches may keep being retried, and the point
/// past which a slow round is *announced* — no longer the point past
/// which a volume is declared dead for slowness, which since the 2026-08-23
/// ruling nothing in this program is: only a device fact ends a volume, and
/// the kernel's `DEADMAN` is what turns a hung device into one.
///
/// **A policy number, and it says so**: nothing about the device supplies one.
/// Five seconds: long enough that a slow stick under a boot's worth of other
/// I/O is not called degraded, short enough that a person watching the console
/// learns about it while they are still watching.
///
/// **It is measured around a syscall, so it is reachable only if the syscall
/// returns** — every bound below it is what decides whether this policy runs at
/// all. There are two of them and they answer different failures. The transport
/// bounds one device round trip (`USB_TIMEOUT_NS`, 2 s in
/// `kernel/src/drivers/xhci`), which is what turns a stick that *stopped
/// answering* into an `Err` here rather than an unbounded wait; that bound is
/// never reached by a device that answers, so on its own it says nothing about
/// how long a call may take. `kernel/src/block.rs`'s `OPERATION` is the other,
/// 2 s over one whole block-device operation — the batching, the retries and
/// the recoveries a single `read_blocks` composes — and it is what bounds a
/// device that answers every transfer and takes too long over the work. Two
/// plus one command's overshoot is what leaves this constant a second to notice
/// with.
///
/// **What it bounds is slowness and not errors, and that split is measured
/// rather than chosen.** The "repeated errors" half of a slowness-and-errors
/// policy does not survive this tree: a failing write is *itself* logged by the driver
/// (`usb-storage: cache flush failed on disk 0`), which commits a kernel record,
/// which is a record this program then tries to write, which fails. Retrying
/// inside a budget therefore does not sample a device that might recover — it
/// runs a feedback loop, measured at **1,737 failing flushes over six seconds**
/// under `usb-flush-fails` before this constant was given the narrower job.
///
/// So an **error about the device** ends it at once, which is what
/// `kernel/src/log_file.rs` did and for a reason that turns out to be this one
/// rather than an idle loop's convenience. What this bounds is a run of budget
/// refusals — a flush the kernel keeps answering `WouldBlock` — so a
/// permanently loaded host does not keep a volume nobody is writing to; and a
/// *successful* round past it is the degradation threshold, where the console
/// learns the volume is slow without the log being thrown away over it. A
/// slow-but-answering device stopped being this constant's to end on
/// 2026-08-23: the kernel's `SYS_FSYNC` now bounds a slow device itself
/// (retry inside `block::DEADMAN`, then a device error), so a round here can
/// outlast this number and still be a durability the machine got.
/// `usb_flush_optional` is the gate for the error half and `--slow-usb` the
/// instrument for the slow one.
pub const LOG_WRITE_BUDGET: Duration = Duration::from_secs(5);

/// Which call refused.
///
/// Named rather than a `&str`, because [`fate`] matches on it: the append and
/// the flush have different answers and a string cannot be matched
/// exhaustively.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// One of the batch's lines did not reach the file.
    Append,
    /// The bytes are in the file and did not reach the device.
    Flush,
    /// Everything answered, and the round took longer than
    /// [`LOG_WRITE_BUDGET`].
    TooSlow,
}

impl Step {
    /// The word this program's console line uses for it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Append => "the append",
            Self::Flush => "the sync",
            Self::TooSlow => "the write",
        }
    }
}

/// What happens to this boot's volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fate {
    /// Keep it, publish nothing for this batch, and try again on the next one.
    /// The bytes are in the file and the next flush covers them.
    Retry,
    /// Keep it, publish this batch — every call answered, so the records are
    /// durable — and say once that the volume is slow. The state the whole
    /// slow-vs-failed split exists for: degraded is not dead, and a log that
    /// arrives late beats a log that was thrown away.
    Degraded,
    /// Stop feeding the volume for the rest of the boot. The log is on the
    /// console only from here.
    GiveUp,
}

/// The decision, from the three things it is a function of.
///
/// `retried_for` is how long the *run* of consecutive retries has lasted, and
/// [`Duration::ZERO`] when this is the first refusal after an answered batch.
/// It is the run and not the round because that is what
/// [`LOG_WRITE_BUDGET`] is about: a round that is refused early costs almost no
/// time, so a per-round check on a permanently loaded host would keep a volume
/// forever.
pub fn fate(step: Step, kind: std::io::ErrorKind, retried_for: Duration) -> Fate {
    match (step, kind) {
        // Nothing was issued, the device is untouched, and the next operation
        // gets a whole fresh budget — as long as the run of them is still
        // inside what a log is worth waiting for.
        (Step::Flush, std::io::ErrorKind::WouldBlock) if retried_for <= LOG_WRITE_BUDGET => {
            Fate::Retry
        }
        // Every call answered: the records are durable, however long the round
        // took, and a volume is never ended on elapsed time — only on the
        // device's own word, which for a hung device the kernel's deadman
        // supplies as an error the arm below sees.
        (Step::TooSlow, _) => Fate::Degraded,
        _ => Fate::GiveUp,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::ErrorKind;

    /// The sighting this exists for: `esp_filesystem`'s `fsync` refused under a
    /// loaded host, on a stick that answered every transfer.
    #[test]
    fn a_flush_that_ran_out_of_budget_keeps_the_volume() {
        assert_eq!(fate(Step::Flush, ErrorKind::WouldBlock, Duration::ZERO), Fate::Retry);
    }

    /// **The one the whole change is measured against.** A stick that cannot
    /// flush is `Other` (`SyscallError::Io`), and ending the boot's log on one
    /// is the shipped behaviour this must not change: the writes are not
    /// durable and no retry makes them so.
    #[test]
    fn a_device_that_cannot_flush_still_ends_the_volume() {
        assert_eq!(fate(Step::Flush, ErrorKind::Other, Duration::ZERO), Fate::GiveUp);
        assert_eq!(fate(Step::Flush, ErrorKind::PermissionDenied, Duration::ZERO), Fate::GiveUp);
        assert_eq!(fate(Step::Flush, ErrorKind::NotFound, Duration::ZERO), Fate::GiveUp);
        assert_eq!(fate(Step::Flush, ErrorKind::OutOfMemory, Duration::ZERO), Fate::GiveUp);
    }

    /// A refused append is a hole in the file, whatever refused it: the records
    /// have left the cursor and there is nothing to re-write.
    #[test]
    fn a_refused_append_ends_the_volume_whichever_word_it_used() {
        assert_eq!(fate(Step::Append, ErrorKind::WouldBlock, Duration::ZERO), Fate::GiveUp);
        assert_eq!(fate(Step::Append, ErrorKind::Other, Duration::ZERO), Fate::GiveUp);
    }

    /// A run of retries is itself the slow volume `LOG_WRITE_BUDGET` bounds, so
    /// a host that stays loaded does not keep a volume nobody is writing to.
    #[test]
    fn a_run_of_retries_longer_than_the_budget_ends_the_volume() {
        let kind = ErrorKind::WouldBlock;
        assert_eq!(fate(Step::Flush, kind, LOG_WRITE_BUDGET), Fate::Retry);
        assert_eq!(
            fate(Step::Flush, kind, LOG_WRITE_BUDGET + Duration::from_millis(1)),
            Fate::GiveUp
        );
    }

    /// A volume that answered every call and took too long over it is
    /// degraded, never dead: the records are durable and elapsed time is not
    /// one of the three evidences a volume may be declared failed on. Until
    /// 2026-08-23 this arm was `GiveUp`, which was declaring death on the
    /// clock — the exact policy the slow-vs-failed split deletes.
    #[test]
    fn a_slow_round_degrades_and_keeps_the_volume() {
        assert_eq!(fate(Step::TooSlow, ErrorKind::Other, Duration::ZERO), Fate::Degraded);
        // However long the run has gone on: slowness never hardens into death
        // here, because for a device that stops answering entirely the
        // kernel's deadman supplies a real error and the GiveUp arm sees it.
        assert_eq!(
            fate(Step::TooSlow, ErrorKind::Other, LOG_WRITE_BUDGET + Duration::from_secs(60)),
            Fate::Degraded
        );
    }

    /// The console words, which the four-way table in `main` is written
    /// against.
    #[test]
    fn every_step_names_itself() {
        assert_eq!(Step::Append.as_str(), "the append");
        assert_eq!(Step::Flush.as_str(), "the sync");
        assert_eq!(Step::TooSlow.as_str(), "the write");
    }
}
