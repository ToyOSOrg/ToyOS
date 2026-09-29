//! `perfstate`: the CPU performance envelope's registers, read back once
//! through the `perf-state` claim its row names. Exits 0 when every CPU holds
//! the kernel's declaration, 1 when one does not or there is no claim.

use std::process::ExitCode;

use toyos::endow;
use toyos::Device;
use toyos_abi::syscall::DeviceType;

fn main() -> ExitCode {
    let Some(claim) = endow::device::<Device>(DeviceType::PerfState) else {
        eprintln!(
            "perfstate: no perf-state claim was endowed; init's log says why, and a machine \
             that declared no performance request has none to give"
        );
        return ExitCode::FAILURE;
    };
    match perfstate::read_back(&claim) {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("perfstate: {why}");
            ExitCode::FAILURE
        }
    }
}
