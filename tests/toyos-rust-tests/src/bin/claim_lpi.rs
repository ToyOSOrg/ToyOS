//! QEMU's `edu` claimed, and told to send its MSI again and again: each
//! message is its claim's one interrupt, read off the claim once the poll on
//! it answers, one record of one interrupt each time.
//!
//! The function masters the bus only once it has a grant, and QEMU sends an
//! MSI as a write the function makes, so one grant comes first.

use std::time::Duration;

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::poller::{Poller, READABLE};
use toyos::syscap::SysCap;
use toyos::volatile::Window;
use toyos::PciDev;
use toyos_abi::syscall::{PciId, SyscallError};

/// `hw/misc/edu.c`: QEMU's educational device.
const EDU: PciId = PciId { vendor: 0x1234, device: 0x11e8 };

/// Its BAR 0: the identification register, and the two that raise and
/// acknowledge its interrupt (`docs/specs/edu.rst`).
const IDENTIFICATION: usize = 0x00;
const RAISE: usize = 0x60;
const ACKNOWLEDGE: usize = 0x64;
/// The identification register's low byte.
const EDU_ID: u32 = 0xed;

/// How many messages the job asks for.
const MESSAGES: u64 = 16;

/// How long one message has to come back as the claim's interrupt: a
/// liveness ceiling.
const BOUND: Duration = Duration::from_secs(10);

fn main() {
    let cap: SysCap = Endowments::get().take(SYSCAP_LABEL).expect("test-runner endows a device-minting capability");
    let dev: PciDev = cap.claim_pci(EDU).unwrap_or_else(|why| panic!("claim_lpi: the claim on edu was refused: {why:?}"));
    let info = dev.describe().expect("claim_lpi: the claim's description");
    let bar = dev.map_bar(0, info.bar_bytes[0]).expect("claim_lpi: BAR 0");
    // SAFETY: the mapping is the BAR's length and lives as long as `bar`.
    let regs = unsafe { Window::new(bar.as_ptr(), info.bar_bytes[0] as usize) };
    let id = regs.read::<u32>(IDENTIFICATION);
    assert_eq!(id & 0xff, EDU_ID, "claim_lpi: BAR 0 answers {id:#010x}, which is not edu's identification");
    let _grant = dev.dma_alloc(4096).expect("claim_lpi: a grant");
    assert_eq!(dev.irq().map(|r| r.count), Err(SyscallError::WouldBlock), "claim_lpi: an interrupt before any was raised");

    let poller = Poller::new(1);
    for sent in 1..=MESSAGES {
        regs.write::<u32>(RAISE, 1);
        poller.watch(&dev, READABLE, 0);
        poller.wait(1, BOUND.as_nanos() as u64, |_| {});
        match dev.irq() {
            Ok(record) => assert_eq!(record.count, 1, "claim_lpi: message {sent} read as {} interrupts", record.count),
            Err(why) => panic!("claim_lpi: message {sent} was not read as the claim's interrupt within {BOUND:?}: {why:?}"),
        }
        regs.write::<u32>(ACKNOWLEDGE, 1);
    }
    assert_eq!(dev.irq().map(|r| r.count), Err(SyscallError::WouldBlock), "claim_lpi: an interrupt no message raised");
    println!("claim_lpi: {MESSAGES} messages edu sent were read as {MESSAGES} interrupts of its claim, one each");
}
