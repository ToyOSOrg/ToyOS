//! Asks init to stop the machine on a kernel armed to refuse the first stop
//! (`power-refused-once`), and says a line once refused. Its verdict is
//! whether that line is in `/log`; `log_after_a_refused_stop` judges it.

use toyos::power::{self, Refused, Stop};
use toyos_abi::syscall::SyscallError;

fn main() {
    let refused = power::stop(Stop::Reboot);
    assert_eq!(refused, Refused::Kernel(SyscallError::NotSupported), "the armed refusal did not answer");
    println!("log refused stop: said after the refusal");
}
