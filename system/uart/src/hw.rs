//! The machine's console UART, as its claim hands it over: a 16550 at the
//! ports its first read binds to this process, or a PL011 in the register
//! frame that read installs. The kernel programmed the line (rate, framing,
//! FIFOs) before userland ran, and nothing here changes it.

use toyos::shm::SharedMemory;
use toyos::volatile::Window;
use toyos_abi::ioport;
use toyos_abi::uart::{UartInfo, NS16550, PL011};

use crate::line::Uart;

/// A 16550's registers, by their offset from its first port (PC16550D).
mod ns16550 {
    /// Receiver buffer, read; transmitter holding register, written.
    pub const DATA: u16 = 0;
    /// Interrupt enable: received data available, and THR empty.
    pub const IER: u16 = 1;
    pub const IER_RDA: u8 = 1 << 0;
    pub const IER_THRE: u8 = 1 << 1;
    /// Interrupt identification: bit 0 clear while an interrupt is pending.
    /// Reading it takes back a THRE interrupt it names.
    pub const IIR: u16 = 2;
    pub const IIR_NONE: u8 = 1 << 0;
    /// Line status: data ready, and THR empty, which in FIFO mode is the
    /// whole transmit FIFO empty.
    pub const LSR: u16 = 5;
    pub const LSR_DR: u8 = 1 << 0;
    pub const LSR_THRE: u8 = 1 << 5;
    /// What the transmit FIFO takes once empty.
    pub const FIFO: usize = 16;
}

/// A PL011's registers, by their offset in its frame (PL011 TRM r1p5, §3.3),
/// which the SBSA generic UART keeps too.
mod pl011 {
    pub const DR: usize = 0x000;
    pub const FR: usize = 0x018;
    pub const FR_RXFE: u32 = 1 << 4;
    pub const FR_TXFF: u32 = 1 << 5;
    /// Interrupt mask set/clear, masked interrupt status, interrupt clear:
    /// the receive, transmit and receive timeout interrupts.
    pub const IMSC: usize = 0x038;
    pub const MIS: usize = 0x040;
    pub const ICR: usize = 0x044;
    pub const RX: u32 = 1 << 4;
    pub const TX: u32 = 1 << 5;
    pub const RT: u32 = 1 << 6;
}

/// The UART the claim named.
pub enum Machine {
    Ns16550 { base: u16, ier: u8 },
    Pl011 { frame: Window, _mapping: SharedMemory },
}

impl Machine {
    /// The UART `info` names, its receiver's interrupt asked for. `None` for a
    /// description this program does not know.
    pub fn open(info: &UartInfo, frame: Option<SharedMemory>) -> Option<Self> {
        let mut machine = match (info.kind, frame) {
            (NS16550, None) => Self::Ns16550 { base: u16::try_from(info.port).ok()?, ier: 0 },
            (PL011, Some(mapping)) => {
                // SAFETY: the mapping is the frame's whole, and is held here
                // for as long as the window is.
                let frame = unsafe { Window::new(mapping.as_ptr(), info.frame_len as usize) };
                Self::Pl011 { frame, _mapping: mapping }
            }
            _ => return None,
        };
        machine.ask_rx();
        Some(machine)
    }

    fn ask_rx(&mut self) {
        match self {
            Self::Ns16550 { base, ier } => {
                *ier |= ns16550::IER_RDA;
                ioport::out8(*base + ns16550::IER, *ier);
            }
            Self::Pl011 { frame, .. } => {
                let mask: u32 = frame.read(pl011::IMSC);
                frame.write(pl011::IMSC, mask | pl011::RX | pl011::RT);
            }
        }
    }
}

impl Uart for Machine {
    fn room(&mut self) -> usize {
        match self {
            Self::Ns16550 { base, .. } => {
                if ioport::in8(*base + ns16550::LSR) & ns16550::LSR_THRE != 0 { ns16550::FIFO } else { 0 }
            }
            Self::Pl011 { frame, .. } => usize::from(frame.read::<u32>(pl011::FR) & pl011::FR_TXFF == 0),
        }
    }

    fn send(&mut self, byte: u8) {
        match self {
            Self::Ns16550 { base, .. } => ioport::out8(*base + ns16550::DATA, byte),
            Self::Pl011 { frame, .. } => frame.write(pl011::DR, u32::from(byte)),
        }
    }

    fn receive(&mut self) -> Option<u8> {
        match self {
            Self::Ns16550 { base, .. } => {
                (ioport::in8(*base + ns16550::LSR) & ns16550::LSR_DR != 0).then(|| ioport::in8(*base + ns16550::DATA))
            }
            Self::Pl011 { frame, .. } => {
                (frame.read::<u32>(pl011::FR) & pl011::FR_RXFE == 0).then(|| frame.read::<u32>(pl011::DR) as u8)
            }
        }
    }

    fn ask_tx(&mut self, on: bool) {
        match self {
            Self::Ns16550 { base, ier } => {
                *ier = if on { *ier | ns16550::IER_THRE } else { *ier & !ns16550::IER_THRE };
                ioport::out8(*base + ns16550::IER, *ier);
            }
            Self::Pl011 { frame, .. } => {
                let mask: u32 = frame.read(pl011::IMSC);
                frame.write(pl011::IMSC, if on { mask | pl011::TX } else { mask & !pl011::TX });
                frame.write(pl011::ICR, pl011::TX);
            }
        }
    }

    fn raising(&mut self) -> bool {
        match self {
            Self::Ns16550 { base, .. } => ioport::in8(*base + ns16550::IIR) & ns16550::IIR_NONE == 0,
            Self::Pl011 { frame, .. } => frame.read::<u32>(pl011::MIS) != 0,
        }
    }
}
