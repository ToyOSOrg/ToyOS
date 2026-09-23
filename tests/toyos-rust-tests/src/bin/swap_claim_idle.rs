//! Take the 82574 a swapped-out netd drove, stop it the way netd does before
//! its first grant, start it mastering, and touch nothing else: the replacement
//! a swap's DMA control puts in netd's place.
//!
//! **What it inherited is said before anything is changed**, so a reader can
//! tell whether the kernel's release reset the part (its receive unit off) or
//! handed it over still running. The control is whether the quiesce alone keeps
//! the part from writing into the previous holder's descriptors once the grant
//! starts it mastering.

use std::time::Duration;

use toyos_abi::syscall::PciId;
use toyos_i219::{regs, Registers};

/// The 82574, QEMU's `e1000e`: netd's Intel driver's other part.
const E82574: PciId = PciId { vendor: 0x8086, device: 0x10d3 };

/// How long it holds the function mastering, for frames to arrive into.
const IDLE: Duration = Duration::from_secs(8);

/// The register window, as volatile 32-bit accesses.
struct Bar(*mut u8);

impl Registers for Bar {
    fn bytes(&self) -> usize {
        regs::REGISTER_BYTES
    }
    fn read(&self, reg: usize) -> u32 {
        // SAFETY: `reg` is a register offset inside the mapped BAR, which is
        // at least `REGISTER_BYTES` long and lives as long as `main`'s mapping.
        unsafe { (self.0.add(reg) as *const u32).read_volatile() }
    }
    fn write(&self, reg: usize, value: u32) {
        // SAFETY: as `read`.
        unsafe { (self.0.add(reg) as *mut u32).write_volatile(value) }
    }
}

fn main() {
    let dev: toyos::PciDev =
        toyos::endow::pci_function(E82574).expect("swap_claim_idle: started holding no 82574");
    let info = dev.describe().expect("swap_claim_idle: the claim's description");
    let (bar, bytes) = info
        .bar_bytes
        .iter()
        .enumerate()
        .find(|(_, bytes)| **bytes >= regs::REGISTER_BYTES as u64)
        .map(|(index, bytes)| (index as u32, *bytes))
        .expect("swap_claim_idle: no register window");
    let mapped = dev.map_bar(bar, bytes).expect("swap_claim_idle: the BAR");
    let regs = Bar(mapped.as_ptr());
    println!(
        "swap_claim_idle: inherited RCTL {:#010x} TCTL {:#010x}",
        regs.read(regs::RCTL),
        regs.read(regs::TCTL)
    );
    toyos_i219::quiesce(&regs);
    let grant = dev.dma_alloc(2 * 1024 * 1024).expect("swap_claim_idle: a grant");
    println!("swap_claim_idle: holding the NIC mastering, its receive and transmit stopped");
    std::thread::sleep(IDLE);
    drop(grant);
    drop(mapped);
    println!("swap_claim_idle: done");
}
