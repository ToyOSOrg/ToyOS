//! A declared row's view holds its own `/state/<name>` and nothing else of
//! `/state` (`toyos_manifest::Program::view`): `state_view`, a job of
//! test-runner, whose row declares `state = true`, writes its row's own state
//! and reaches neither a service's nor the supervisor's grants store.
//!
//! On QEMU x86-64 (`tests/images/testcases.toml`): what it claims is which
//! grants the supervisor minted and the file server honours, completion and
//! contents, with no hardware in it.

use std::time::Duration;

use super::{compile, qemu};
use qemu::{BootOptions, QemuInstance};

const JOB: &str = "state_view";
const BOUND: Duration = Duration::from_secs(60);

pub fn a_row_reaches_its_own_state_and_no_other() -> Result<(), String> {
    let bin = qemu::build_toyos_bin(qemu::SUITE_ARCH, &compile::repo_root().join("tests/guest/rust-tests"), JOB);
    let case = compile::repo_root().join("tests/images/testcases.toml");
    let mut qemu = QemuInstance::boot_with_options(&case, &[], &[(JOB.to_string(), bin)], BootOptions::default());
    let result = qemu.run_test(&format!("test_rs_{JOB}"), BOUND);
    if result.error.is_some() || result.exit_code != Some(0) {
        return Err(format!("{JOB} ended {:?} {:?}:\n{}", result.exit_code, result.error, result.serial));
    }
    eprintln!("  [state] a job reached its row's own /state/test-runner and nothing else of /state");
    Ok(())
}
