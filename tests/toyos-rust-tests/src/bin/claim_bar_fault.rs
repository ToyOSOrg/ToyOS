//! A claimed function's BAR, mapped into its holder, is no memory the kernel
//! reads or writes for that holder: a fault whose frame pointer is in it, and
//! a fault at an address in it, each end their process with the kernel's
//! crash report reading nothing there, and a syscall naming it as a buffer is
//! refused `BadAddress` with nothing read or written.
//!
//! The function is QEMU's `edu`, whose DMA source and destination registers
//! (`docs/specs/edu.rst`) are eight bytes each that keep what is written to
//! them across claims, since nothing resets it. This job writes one marker in
//! each and never reads them into a register a fault could report: the
//! harness reds on either marker anywhere on the console, which is where a
//! report that read through the BAR would put it.

#[path = "../arch/stack.rs"]
mod stack;

use std::os::toyos::process::CommandExt;
use std::process::Command;

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::shm::SharedMemory;
use toyos::syscap::SysCap;
use toyos::volatile::Window;
use toyos::PciDev;
use toyos_abi::syscall::{self, PciId, SyscallError};

const SELF_PATH: &str = "/system/bin/test_rs_claim_bar_fault";

/// `hw/misc/edu.c`: QEMU's educational device.
const EDU: PciId = PciId { vendor: 0x1234, device: 0x11e8 };

/// The DMA source register, and the destination after it: a frame pointer
/// here names a saved frame pointer of `MARK_SOURCE` and a return address of
/// `MARK_DESTINATION`.
const DMA_SOURCE: usize = 0x80;
const DMA_DESTINATION: usize = 0x88;

/// What the two registers hold while the children fault, which
/// `tests/toyos.rs`'s `virt_claim_lpi` reds on: neither a user address nor
/// aligned, so a report that read one as a frame pointer stops there.
const MARK_SOURCE: u64 = 0xbad0_c0de_0000_5eed;
const MARK_DESTINATION: u64 = 0xbad1_c0de_0000_5eed;

fn main() {
    let role = std::env::args().nth(1);
    let Some(role) = role.as_deref() else { return holder() };
    // Both held across the fault, which never returns.
    let dev = claim(&endowed());
    let (bar, _) = mapped_registers(&dev);
    let at = bar.as_ptr() as u64 + DMA_SOURCE as u64;
    match role {
        "frame" => stack::undefined_with_stack_at(at, at),
        "jump" => {
            // SAFETY: none — the fetch from a BAR mapped never executable is
            // the fault, and it ends this process.
            let into: extern "C" fn() -> ! = unsafe { core::mem::transmute(at as usize) };
            into()
        }
        other => panic!("claim_bar_fault: unknown role {other:?}"),
    }
}

fn endowed() -> SysCap {
    Endowments::get().take(SYSCAP_LABEL).expect("claim_bar_fault: test-runner endows a device-minting capability")
}

fn claim(cap: &SysCap) -> PciDev {
    cap.claim_pci(EDU).unwrap_or_else(|why| panic!("claim_bar_fault: the claim on edu was refused: {why:?}"))
}

/// BAR 0 mapped, and its registers.
fn mapped_registers(dev: &PciDev) -> (SharedMemory, Window) {
    let info = dev.describe().expect("claim_bar_fault: the claim's description");
    let bar = dev.map_bar(0, info.bar_bytes[0]).expect("claim_bar_fault: BAR 0");
    // SAFETY: the mapping is BAR 0's length and lives as long as `bar`, which
    // every caller holds as long as the window.
    let regs = unsafe { Window::new(bar.as_ptr(), info.bar_bytes[0] as usize) };
    (bar, regs)
}

fn holder() {
    let cap = endowed();
    {
        let dev = claim(&cap);
        let (bar, regs) = mapped_registers(&dev);
        let at = bar.as_ptr() as u64 + DMA_SOURCE as u64;
        regs.write::<u64>(DMA_SOURCE, MARK_SOURCE);
        regs.write::<u64>(DMA_DESTINATION, MARK_DESTINATION);

        let pipe = syscall::pipe().expect("claim_bar_fault: a pipe");
        assert_eq!(syscall::write(pipe.write, &[0x5a; 16]), Ok(16), "claim_bar_fault: the pipe took no bytes");
        // SAFETY: the sixteen bytes are the two registers, inside the live
        // mapping; the slice only carries their address into the syscall.
        let registers = unsafe { core::slice::from_raw_parts_mut(at as *mut u8, 16) };
        assert_eq!(
            syscall::read(pipe.read, registers),
            Err(SyscallError::BadAddress),
            "claim_bar_fault: a read into the BAR was not refused BadAddress"
        );
        assert_eq!(
            syscall::write(pipe.write, registers),
            Err(SyscallError::BadAddress),
            "claim_bar_fault: a write from the BAR was not refused BadAddress"
        );
        assert!(
            regs.read::<u64>(DMA_SOURCE) == MARK_SOURCE && regs.read::<u64>(DMA_DESTINATION) == MARK_DESTINATION,
            "claim_bar_fault: the refused read wrote into the BAR"
        );
        syscall::close(pipe.read);
        syscall::close(pipe.write);
        println!("claim_bar_fault: a read into the BAR and a write from it were refused BadAddress, the registers untouched");
    }

    for role in ["frame", "jump"] {
        let status = Command::new(SELF_PATH)
            .arg(role)
            .endow(SYSCAP_LABEL, cap.duplicate().expect("claim_bar_fault: a capability for the child").into_raw().0)
            .status()
            .expect("claim_bar_fault: spawn a child");
        assert_eq!(status.code(), Some(-1), "claim_bar_fault: the {role} child ended {status:?}, not by its fault");
        println!("claim_bar_fault: the {role} child's fault in the BAR ended it");
    }

    // The markers were there for both reports to read.
    let dev = claim(&cap);
    let (_bar, regs) = mapped_registers(&dev);
    assert!(
        regs.read::<u64>(DMA_SOURCE) == MARK_SOURCE && regs.read::<u64>(DMA_DESTINATION) == MARK_DESTINATION,
        "claim_bar_fault: the registers lost their markers while the children faulted"
    );
    println!("claim_bar_fault: two faults in the BAR ended their processes, and a syscall naming it was refused");
}
