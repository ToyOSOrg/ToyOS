//! A `perf-state` read no CPU but its asker answers (`perf-state-deaf-cpu`,
//! which also grants the claim where no request was declared): refused `Io`
//! once the kernel's bound has passed, never answered and never left waiting.
//! A read that does not wait goes first, and each read after it must make an
//! ask of its own. `perf_state_silent_cpu` drives it and reads which ask and
//! which CPU the kernel named.

use toyos::endow::Endowments;
use toyos::syscap::SysCap;
use toyos::{AsHandle, Device};
use toyos_abi::perf::answer_len;
use toyos_abi::syscall::{self, DeviceType, SyscallError, SYSCAP_LABEL};

fn main() {
    let cap: SysCap = Endowments::get()
        .take(SYSCAP_LABEL)
        .expect("the test estate is endowed a device-minting capability");
    let claim = cap
        .claim::<Device>(DeviceType::PerfState)
        .expect("perf-state-deaf-cpu grants the claim on any machine");
    let mut buf = vec![0u8; answer_len(syscall::cpu_count() as usize)];
    assert_eq!(syscall::read_nonblock(claim.as_handle(), &mut buf), Err(SyscallError::WouldBlock));
    // Twice: the claim still answers after a refusal.
    for _ in 0..2 {
        assert_eq!(claim.read(&mut buf), Err(SyscallError::Io));
    }
    println!("===PERF_STATE_SILENT_OK===");
}
