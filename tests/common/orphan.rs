//! A guest a `SIGKILL`ed harness would leave running: the owner, and the test
//! that kills it and watches its QEMU go.

use std::io::Read;
use std::path::Path;
use std::process::Command;

use toyos_build::tether::Owner;
use toyos_build::testargs;
use toyos_tmpdir::TempDir;

use super::qemu::{self, BootOptions, QemuInstance, Staged};

/// What the owner prints its QEMU's pid after.
const HELD: &str = "held: qemu ";

/// `--hold <image>`: boot `image`, print its QEMU's pid, and hold it until
/// stdin ends.
pub fn hold(test_config: &Path, image: &Path) {
    let options = BootOptions { boot_image: Some(Staged::Pristine(image.to_path_buf())), ..Default::default() };
    let guest = QemuInstance::boot_with_options(test_config, &[], &[], options);
    println!("{HELD}{}", guest.pid());
    std::io::stdin().read_to_end(&mut Vec::new()).expect("read the owner's stdin");
}

/// The harness's `SIGKILL` ends its guest, whose QEMU inherited `SIGHUP`
/// blocked and ignored.
pub fn guest_dies_with_its_harness(test_config: &Path) -> Result<(), String> {
    // The owner's `$TMPDIR` and the image it boots: what its `SIGKILL` leaves
    // there goes when this does, after the owner.
    let tmp = TempDir::new("orphan");
    let image = tmp.join("boot.img");
    std::fs::write(&image, qemu::build_boot_image(test_config, &[], &[], &[]))
        .map_err(|e| format!("write {}: {e}", image.display()))?;
    let mut owner = Command::new(std::env::current_exe().unwrap());
    owner.arg(testargs::HOLD.name).arg(&image).env("TMPDIR", &tmp);
    let mut owner = Owner::spawn(owner)?;
    // The owner builds nothing, so its one wait is its boot, whose own ceiling
    // ends it well inside the backstop on any wait on a guest.
    let pid: u32 =
        owner.said(HELD, qemu::GUEST_WEDGED)?.parse().map_err(|e| format!("the owner's QEMU pid: {e}"))?;
    let took = owner.killed(&[pid])?;
    eprintln!("  [orphan] QEMU {pid} gone {took:?} after its harness's SIGKILL");
    Ok(())
}
