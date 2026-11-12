//! AArch64 applies nothing for a program that asks for isolated speculation:
//! no decision names what Arm prescribes
//! (`issues/no-aarch64-program-runs-with-speculative-store-bypass-disabled.md`).

use kernel::mitigation::Speculation;
use toyos_abi::Pid;

/// x86-64's applies what a switch owes a program that asked, and nothing is
/// applied here.
pub fn switch_to(_next: Option<(Pid, Speculation)>) {}

/// x86-64's reads `IA32_SPEC_CTRL`, and there is none here.
#[cfg(feature = "test-actuators")]
pub fn debug_spec_ctrl() -> Option<u64> {
    None
}

/// x86-64's names the bits it sets on request, and none are set here.
#[cfg(feature = "test-actuators")]
pub fn debug_spec_ctrl_on_request() -> u64 {
    0
}

/// x86-64's counts its IBPBs, and none is issued here.
#[cfg(feature = "test-actuators")]
pub fn debug_switch_ibpbs() -> Option<u64> {
    None
}
