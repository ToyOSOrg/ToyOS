//! A `perf-state` read no CPU but its asker answers (`perf-state-deaf-cpu`,
//! which also grants the claim where no request was declared): refused `Io`
//! once the kernel's bound has passed, never answered and never left waiting.
//! A read that does not wait goes first, and each read after it must make an
//! ask of its own. The boot's fourth ask is every CPU's to answer, and each
//! record names the CPU that read it. `perf_state_silent_cpu` drives it, reads
//! which ask and which CPU the kernel named, and holds each record's identity
//! to the kernel's roster.

use toyos::endow::Endowments;
use toyos::syscap::SysCap;
use toyos::{AsHandle, Device};
use toyos_abi::perf::{answer_len, CpuRegisters};
use toyos_abi::syscall::{self, DeviceType, SyscallError, SYSCAP_LABEL};

fn main() {
    let cap: SysCap = Endowments::get()
        .take(SYSCAP_LABEL)
        .expect("the test estate is endowed a device-minting capability");
    let claim = cap
        .claim::<Device>(DeviceType::PerfState)
        .expect("perf-state-deaf-cpu grants the claim on any machine");
    let cpus = syscall::cpu_count() as usize;
    let mut buf = vec![0u8; answer_len(cpus)];
    assert_eq!(syscall::read_nonblock(claim.as_handle(), &mut buf), Err(SyscallError::WouldBlock));
    for _ in 0..2 {
        assert_eq!(claim.read(&mut buf), Err(SyscallError::Io));
    }
    // The claim still answers after a refusal.
    let whole = buf.len();
    assert_eq!(claim.read(&mut buf), Ok(whole));
    for cpu in 0..cpus {
        let regs = CpuRegisters::read_from(&buf[answer_len(cpu)..])
            .expect("the buffer holds every CPU's record");
        println!("cpu{cpu} hardware_id={}", regs.hardware_id);
    }
    println!("===PERF_STATE_SILENT_OK===");
}
