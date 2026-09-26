//! The console UART: whichever PL011-shaped UART the SPCR places firmware's
//! console on. Firmware configured it (rate, framing, enable), and SPCR
//! describes that configuration, so this file programs nothing and only moves
//! bytes: a PL011's `UARTDR` and `UARTFR`, which the SBSA generic UART keeps
//! at the same offsets (Arm PL011 TRM r1p5, §3.3; Arm BSA 1.0, Appendix B).

use core::sync::atomic::{AtomicU64, Ordering};

use toyos_acpi::{SerialInterface, GAS_SYSTEM_MEMORY};

use crate::drivers::acpi::DirectPhys;
use crate::log;
use crate::mm::{DirectMap, Mmio};

/// `UARTDR`, the data register, and `UARTFR`, the flag register.
const DR: u64 = 0x000;
const FR: u64 = 0x018;
/// `UARTFR.RXFE`: the receive FIFO is empty. `UARTFR.TXFF`: the transmit FIFO is full.
const FR_RXFE: u32 = 1 << 4;
const FR_TXFF: u32 = 1 << 5;
/// One 4 KiB register frame, which is what both UARTs decode.
const FRAME: u64 = 0x1000;

/// The register frame's physical address; zero until [`init`] found one.
static BASE: AtomicU64 = AtomicU64::new(0);

fn regs() -> Mmio {
    let base = BASE.load(Ordering::Relaxed);
    assert!(base != 0, "console UART: a byte moved before `init` found the UART");
    Mmio::new(DirectMap::from_phys(base), FRAME)
}

/// Find the UART SPCR names and answer whether it is one this file drives.
pub fn init(rsdp_addr: u64) -> bool {
    let spcr = match toyos_acpi::spcr(DirectPhys, rsdp_addr) {
        Ok(spcr) => spcr,
        Err(e) => {
            log!("serial: no console UART, because the SPCR is unusable: {e:?}");
            return false;
        }
    };
    let kind = match spcr.interface {
        SerialInterface::Pl011 => "PL011",
        SerialInterface::SbsaGeneric | SerialInterface::SbsaGeneric32 => "SBSA generic UART",
        SerialInterface::Ns16550 | SerialInterface::Other(_) => {
            log!("serial: SPCR names a {:?} UART, which this kernel does not drive on AArch64", spcr.interface);
            return false;
        }
    };
    if spcr.base.space != GAS_SYSTEM_MEMORY || spcr.base.address == 0 {
        log!("serial: SPCR places the {kind} at {:?}, which is not a memory-mapped frame", spcr.base);
        return false;
    }
    BASE.store(spcr.base.address, Ordering::Relaxed);
    log!("serial: {kind} at {:#x} (SPCR, GSIV {})", spcr.base.address, spcr.gsiv);
    true
}

/// Whether a received byte waits.
pub fn rx_ready() -> bool {
    regs().read_u32(FR) & FR_RXFE == 0
}

/// The received byte; only after [`rx_ready`] said one waits.
pub fn read_byte() -> u8 {
    regs().read_u32(DR) as u8
}

/// Whether the transmitter will take a byte.
pub fn tx_ready() -> bool {
    regs().read_u32(FR) & FR_TXFF == 0
}

/// Put one byte in the transmitter; only after [`tx_ready`], or the byte may be lost.
pub fn write_byte(byte: u8) {
    regs().write_u32(DR, u32::from(byte));
}
