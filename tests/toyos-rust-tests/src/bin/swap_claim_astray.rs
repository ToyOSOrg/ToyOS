//! Take the 82574 a swapped-out netd drove, aim its receive ring outside the
//! one grant this claim holds, and wait on the claim: the replacement the
//! refusal control puts in netd's place.
//!
//! **The first frame the host sends makes the part fetch a descriptor from an
//! address its domain does not map**, so the unit refuses it and the claim
//! faults. The part's interrupts stay masked, so nothing but that fault can
//! wake this program; what it then reads from the claim is the verdict.
//!
//! It exits 1 on the refusal it waits for, and 2 when [`TOLD_WITHIN`] passed
//! or a wait on the claim ended with nothing ready: a refusal read after an
//! unwoken wait is one nobody was woken for.

use std::time::{Duration, Instant};

use toyos::poller::{Poller, READABLE};
use toyos_abi::syscall::{PciId, SyscallError};
use toyos_i219::{regs, Registers};

/// The 82574, QEMU's `e1000e`: netd's Intel driver's other part.
const E82574: PciId = PciId { vendor: 0x8086, device: 0x10d3 };

/// Where the ring is aimed, past the grant: further than a claim may ever be
/// granted in total, so nothing this claim holds is there.
const ASTRAY: u64 = 64 * 1024 * 1024;

/// A liveness bound on the host's frames and the kernel's wake, never a
/// measurement.
const TOLD_WITHIN: Duration = Duration::from_secs(20);

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
        toyos::endow::pci_function(E82574).expect("swap_claim_astray: started holding no 82574");
    let info = dev.describe().expect("swap_claim_astray: the claim's description");
    let (bar, bytes) = info
        .bar_bytes
        .iter()
        .enumerate()
        .find(|(_, bytes)| **bytes >= regs::REGISTER_BYTES as u64)
        .map(|(index, bytes)| (index as u32, *bytes))
        .expect("swap_claim_astray: no register window");
    let mapped = dev.map_bar(bar, bytes).expect("swap_claim_astray: the BAR");
    let regs = Bar(mapped.as_ptr());
    toyos_i219::quiesce(&regs);
    let grant = dev.dma_alloc(toyos_i219::GRANT_BYTES).expect("swap_claim_astray: a grant");
    let ring = grant.device_addr + ASTRAY;
    regs.write(regs::RDBAL, ring as u32);
    regs.write(regs::RDBAH, (ring >> 32) as u32);
    regs.write(regs::RDLEN, 4096);
    regs.write(regs::RDH, 0);
    regs.write(regs::RDT, 255);
    regs.write(
        regs::RCTL,
        regs::rctl::EN | regs::rctl::BAM | regs::rctl::BSIZE_2048 | regs::rctl::SECRC,
    );
    println!("swap_claim_astray: holding the NIC mastering, its receive ring at {ring:#x}, outside its grant");

    let poller = Poller::new(1);
    let asked = Instant::now();
    loop {
        match dev.irq() {
            Ok(_) | Err(SyscallError::WouldBlock) => {}
            Err(refused) => {
                println!("swap_claim_astray: its claim refused the interrupt read: {refused:?}");
                std::process::exit(1);
            }
        }
        let Some(left) = TOLD_WITHIN.checked_sub(asked.elapsed()) else {
            println!("swap_claim_astray: its claim refused nothing in {TOLD_WITHIN:?}");
            std::process::exit(2);
        };
        poller.watch(&dev, READABLE, 0);
        let mut woken = false;
        poller.wait(1, left.as_nanos() as u64, |_| woken = true);
        if !woken {
            println!("swap_claim_astray: its claim refused nothing: no wake in {TOLD_WITHIN:?}");
            std::process::exit(2);
        }
    }
}
