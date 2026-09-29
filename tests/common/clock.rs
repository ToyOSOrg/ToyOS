//! Telling a suspended host from a slow one.
//!
//! The dev machine is a laptop and the owner closes the lid. A run that spans
//! that is not a slow run, it is an **invalid measurement**: QEMU's virtual
//! clock, the guest's own millisecond stamps and every device timing in it jump
//! by however long the machine was away, and every wall-clock ceiling in the
//! suite is taken against one of those. CLAUDE.md already
//! documents the signature — a tight cluster of durations plus a few enormous
//! outliers — and documents it as something an agent must check *before*
//! recording a finding, which is to say the harness has never been able to.
//!
//! It can, and with no new source of truth: the two clocks the harness already
//! reads disagree by exactly the suspended time.
//!
//! - `Instant` is `CLOCK_UPTIME_RAW` on this platform and `CLOCK_MONOTONIC` on
//!   Linux. Neither advances while the machine is asleep — the first by its
//!   documented definition, which `library/std/src/sys/time/unix.rs` quotes in
//!   full beside the constant.
//! - `SystemTime` is the wall clock and does.
//!
//! So `wall − monotonic` over an interval **is** the time the host spent
//! stopped, and every deadline the suite takes is on the monotonic one — which
//! is why a suspended run does not report a timeout. It reports whatever the
//! guest made of two hours in the middle, and calls it a defect.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

/// A reading of both clocks at one moment.
#[derive(Clone, Copy)]
pub struct Mark {
    wall: SystemTime,
    mono: Instant,
    artifact_build: toyos_build::build::ArtifactBuildMark,
}

/// Below this, the pair is drifting rather than jumping.
///
/// A suspend is minutes; NTP slews rather than steps in ordinary operation, and
/// a step large enough to reach this is itself a reason to distrust a timing
/// verdict. Two seconds is generous against both and small against the thing it
/// detects.
pub const SUSPENDED_AT_LEAST: Duration = Duration::from_secs(2);

/// **Nothing else can reach this state.** The two clocks diverge only when the
/// machine actually stops, and a process cannot suspend the host it runs on —
/// the lid is what produces it and there is no API that asks for it. So the
/// actuator is the clock source, and it moves the wall clock alone, which is
/// precisely and only what a suspend does to this pair: a staged reading is
/// indistinguishable from a real one for every consumer downstream, verdict and
/// message alike.
pub(crate) static STAGED: AtomicU64 = AtomicU64::new(0);

pub fn mark() -> Mark {
    let staged = Duration::from_millis(STAGED.load(Ordering::SeqCst));
    Mark {
        wall: SystemTime::now() + staged,
        mono: Instant::now(),
        artifact_build: toyos_build::build::mark_artifact_build_time(),
    }
}

impl Mark {
    /// Monotonic execution time since this mark.
    ///
    /// Suspend is already excluded because the monotonic clock stops with the
    /// host. Construction of a memoized boot artifact is excluded explicitly:
    /// it is a cold-cache cost shared by the shard, not the repeatable cost of
    /// whichever test happened to request that kernel or ROOT image first. Fresh
    /// per-boot image creation and the boot itself remain in this duration.
    pub fn elapsed(&self) -> Duration {
        self.artifact_build.execution_part(self.mono.elapsed())
    }

    /// How long the host was stopped between this mark and now.
    ///
    /// Saturating in both directions on purpose: a wall clock that went
    /// *backwards* is not a suspend and has nothing to say here.
    ///
    /// Reads both clocks, so it is never exactly zero — the two are two
    /// syscalls and the gap between them lands in the answer. That is what
    /// [`SUSPENDED_AT_LEAST`] is a threshold against, and asking this twice
    /// gives two different sub-microsecond answers.
    pub fn suspended(&self) -> Duration {
        let now = mark();
        let wall = now.wall.duration_since(self.wall).unwrap_or(Duration::ZERO);
        wall.saturating_sub(now.mono.duration_since(self.mono))
    }
}
