//! The `isa` claim across the i8042's quarantine: refused while the kernel
//! drives the controller, granted once the quarantine has let it go, and the
//! controller then the holder's alone.
//!
//! Run under `isa-claim-straddles-quarantine`, which raises the driver's flood
//! at each answered claim and holds the quarantine between its two steps until
//! a claim has been answered there. The granted claim goes to `isa_lines`'
//! `device` role, which drives the controller to an interrupt and its byte.

use std::os::toyos::process::CommandExt;
use std::process::Command;
use std::time::{Duration, Instant};

use toyos::endow::Endowments;
use toyos::syscap::SysCap;
use toyos::Device;
use toyos_abi::syscall::{IsaId, SyscallError, SYSCAP_LABEL};

/// `isa_lines`' label for the claim its `device` role takes.
const CLAIM_LABEL: &str = "isa-claim";

/// The ceiling on the quarantine: it takes two scheduler passes of the CPU the
/// controller's vector is pinned to.
const LET_GO: Duration = Duration::from_secs(20);

fn main() {
    let cap: SysCap = Endowments::get()
        .take(SYSCAP_LABEL)
        .expect("the test estate is endowed a device-minting capability");
    let set = IsaId::parse("0060,0064:1,12").expect("the i8042's set");
    let by = Instant::now() + LET_GO;
    let mut refused = 0u32;
    let claim: Device = loop {
        match cap.claim_isa(set) {
            Ok(claim) => break claim,
            Err(SyscallError::PermissionDenied) => refused += 1,
            Err(other) => panic!("isa straddled: a claim answered {other:?}"),
        }
        assert!(Instant::now() < by, "isa straddled: the kernel never let the controller go in {LET_GO:?}");
        std::thread::yield_now();
    };
    assert!(refused > 0, "isa straddled: the first claim was granted, so the kernel never drove the controller");
    println!("isa straddled: {refused} claim(s) refused before the kernel let the controller go");

    let status = Command::new("/system/bin/test_rs_isa_lines")
        .arg("device")
        .endow(CLAIM_LABEL, claim.into_raw().0)
        .status()
        .expect("isa straddled: spawn the holder");
    assert!(status.success(), "isa straddled: the holder ended {:?}", status.code());
    println!("isa straddled: the controller the quarantine let go answered its holder alone");
}
