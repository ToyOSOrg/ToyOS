//! A guest a `SIGKILL`ed harness would leave running: the owner, and the test
//! that kills it and watches its QEMU go.

use std::io::Read;
use std::path::Path;
use std::process::Command;

use toyos_build::tether::Owner;
use toyos_build::testargs;

use super::qemu::QemuInstance;

/// What the owner prints its QEMU's pid after.
const HELD: &str = "held: qemu ";

/// `--hold`: boot one guest, print its QEMU's pid, and hold it until stdin
/// ends.
pub fn hold(test_config: &Path) {
    let guest = QemuInstance::boot(test_config, &[], &[]);
    println!("{HELD}{}", guest.pid());
    std::io::stdin().read_to_end(&mut Vec::new()).expect("read the owner's stdin");
}

/// The harness's `SIGKILL` ends its guest, whose QEMU inherited `SIGHUP`
/// blocked and ignored.
pub fn guest_dies_with_its_harness() -> Result<(), String> {
    let mut owner = Command::new(std::env::current_exe().unwrap());
    owner.arg(testargs::HOLD.name);
    let mut owner = Owner::spawn(owner)?;
    let pid: u32 = owner.said(HELD)?.parse().map_err(|e| format!("the owner's QEMU pid: {e}"))?;
    let took = owner.killed(&[pid])?;
    eprintln!("  [orphan] QEMU {pid} gone {took:?} after its harness's SIGKILL");
    Ok(())
}
