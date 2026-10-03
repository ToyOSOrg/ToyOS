//! The `isa` row once its last holder has given it back.
//!
//! A holder's exit is not its claim's release having run: the release is a
//! deferred hook another CPU can still be inside when `wait` returns
//! (`issues/kernel/deferred-release-outlives-its-syscall.md`). So the next
//! claim is asked for until it is granted.

use std::time::{Duration, Instant};

use toyos::syscap::SysCap;
use toyos::Device;
use toyos_abi::syscall::{IsaId, SyscallError};

/// The liveness ceiling on a release in flight, and it panics by name.
const RELEASED: Duration = Duration::from_secs(5);

/// The claim on `set`, once `holder`, which held the row before, has let it go.
pub fn claim_after(cap: &SysCap, set: IsaId, holder: &str) -> Device {
    let by = Instant::now() + RELEASED;
    loop {
        match cap.claim_isa(set) {
            Ok(claim) => return claim,
            Err(SyscallError::AlreadyExists) => {}
            Err(other) => panic!("isa: the claim after {holder} answered {other:?}"),
        }
        assert!(Instant::now() < by, "isa: the row never came back from {holder} in {RELEASED:?}");
        std::thread::yield_now();
    }
}
