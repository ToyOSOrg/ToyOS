//! The console UART: whichever PL011-shaped UART the SPCR places firmware's
//! console on. Firmware configured it (rate, framing, enable), and SPCR
//! describes that configuration, so this file programs nothing and only moves
//! bytes: a PL011's `UARTDR` and `UARTFR`, which the SBSA generic UART keeps
//! at the same offsets (Arm PL011 TRM r1p5, §3.3; Arm BSA 1.0, Appendix B).
//! Its claim ([`claim`]) hands the holder the register frame and the SPI.

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use toyos_abi::uart::{UartInfo, PL011};
use toyos_acpi::{SerialInterface, GAS_SYSTEM_MEMORY};

use crate::device::ClaimError;
use crate::drivers::acpi::direct_phys;
use crate::drivers::serial::Registers;
use crate::log;
use crate::mm::policy::CachePolicy;
use crate::mm::{DirectMap, Mmio};
use crate::object::device::DeviceInfo;
use crate::object::shm::SharedMemObject;

/// `UARTDR`, the data register, and `UARTFR`, the flag register.
const DR: u64 = 0x000;
const FR: u64 = 0x018;
/// `UARTFR.TXFF`: the transmit FIFO is full.
const FR_TXFF: u32 = 1 << 5;
/// Bytes the transmitter takes once [`tx_ready`] says so: `TXFF` clear promises
/// room for one, whatever the FIFO's depth.
pub const TX_BURST: usize = 1;
/// One 4 KiB register frame, which is what both UARTs decode.
const FRAME: u64 = 0x1000;

/// The register frame's physical address; zero until [`init`] found one.
static BASE: AtomicU64 = AtomicU64::new(0);

/// The UART's SPI, as the SPCR names it; zero for none.
static GSIV: AtomicU32 = AtomicU32::new(0);

/// `isa`'s row for the console UART.
const UART_ROW: usize = 2;

/// The register frame's physical address, once [`init`] found one: what the
/// kernel's own tables map before they replace the loader's.
pub fn frame() -> Option<u64> {
    let base = BASE.load(Ordering::Relaxed);
    (base != 0).then_some(base)
}

fn regs() -> Mmio {
    let base = BASE.load(Ordering::Relaxed);
    assert!(base != 0, "console UART: a byte moved before `init` found the UART");
    Mmio::new(DirectMap::from_phys(base), FRAME)
}

/// Find the UART SPCR names and answer whether it is one this file drives.
/// Takes the registers for the call it shares with the 16550's `init`, which
/// programs its UART; this one touches none.
pub fn init(_: &mut Registers, rsdp_addr: u64) -> bool {
    let spcr = match toyos_acpi::spcr(direct_phys(), rsdp_addr) {
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
    GSIV.store(spcr.gsiv, Ordering::Relaxed);
    log!("serial: {kind} at {:#x} (SPCR, GSIV {})", spcr.base.address, spcr.gsiv);
    true
}

/// Fill the console UART's row, where the boot found one: no port, and its
/// interrupt, which a PL011 raises active-high and holds until its cause is
/// cleared (Arm PL011 TRM r1p5, "Interrupts"), so a level SPI. Only its
/// class claims it.
pub fn fill_row() {
    let gsiv = GSIV.load(Ordering::Relaxed);
    if frame().is_none() {
        return;
    }
    if gsiv < 32 {
        return log!("isa: no console UART row — the SPCR names its interrupt GSIV {gsiv}, which is no SPI");
    }
    crate::isa::fill(
        UART_ROW,
        crate::isa::Function {
            name: "the console UART",
            runs: alloc::vec![],
            irqs: alloc::vec![],
            wires: alloc::vec![super::pio::Wire { intid: gsiv, level: true }],
        },
    );
}

/// The claim on the UART's row: its register frame, mapped uncached, and its
/// SPI.
pub fn claim() -> Result<(crate::isa::Row, DeviceInfo), ClaimError> {
    let base = frame().ok_or(ClaimError::Absent)?;
    // A frame that does not start a page is one no mapping hands over alone.
    if base % FRAME != 0 {
        return Err(ClaimError::Unusable);
    }
    let row = crate::isa::claim_row(UART_ROW)?;
    let frame = SharedMemObject::aperture(base, FRAME, CachePolicy::Uncacheable)?;
    let info = UartInfo {
        kind: PL011,
        port: 0,
        frame: toyos_abi::HANDLE_INVALID,
        frame_len: FRAME as u32,
        level: 1,
    };
    Ok((row, DeviceInfo::Uart(info, Some(frame))))
}

/// Whether the transmitter will take a byte.
pub fn tx_ready(_: &mut Registers) -> bool {
    regs().read_u32(FR) & FR_TXFF == 0
}

/// Put one byte in the transmitter; only after [`tx_ready`], or the byte may be lost.
pub fn write_byte(_: &mut Registers, byte: u8) {
    regs().write_u32(DR, u32::from(byte));
}
