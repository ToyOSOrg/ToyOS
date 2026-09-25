//! Take the 82574 a swapped-out netd drove exactly as it was left — its
//! receive unit still on and aimed at netd's rings — and start it mastering
//! with one grant of netd's own size: the replacement the residue control puts
//! in netd's place.
//!
//! **It does not stop the part.** On the T14 the I219 wrote into its previous
//! holder's buffers after netd's replacement had stopped it, because no
//! register write retracts a frame the function had already taken in; QEMU's
//! part holds no such frame, so a receive unit left on is how this machine
//! stages the same write.
//!
//! **It holds the part mastering until it is killed**: the host ends the
//! window on the frames it sent, and the boot ends under it.

use toyos_abi::syscall::PciId;
use toyos_i219::{regs, Registers};

/// The 82574, QEMU's `e1000e`: netd's Intel driver's other part.
const E82574: PciId = PciId { vendor: 0x8086, device: 0x10d3 };

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
        toyos::endow::pci_function(E82574).expect("swap_claim_running: started holding no 82574");
    let info = dev.describe().expect("swap_claim_running: the claim's description");
    let (bar, bytes) = info
        .bar_bytes
        .iter()
        .enumerate()
        .find(|(_, bytes)| **bytes >= regs::REGISTER_BYTES as u64)
        .map(|(index, bytes)| (index as u32, *bytes))
        .expect("swap_claim_running: no register window");
    let mapped = dev.map_bar(bar, bytes).expect("swap_claim_running: the BAR");
    let regs = Bar(mapped.as_ptr());
    println!(
        "swap_claim_running: inherited RCTL {:#010x} TCTL {:#010x}",
        regs.read(regs::RCTL),
        regs.read(regs::TCTL)
    );
    let _grant = dev.dma_alloc(toyos_i219::GRANT_BYTES).expect("swap_claim_running: a grant");
    println!("swap_claim_running: holding the NIC mastering, its receive unit as netd left it");
    loop {
        std::thread::park();
    }
}
