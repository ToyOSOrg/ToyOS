//! The console UART: the 16550 at COM1's I/O ports, where every PC this
//! kernel boots puts one if it has one.

use super::cpu::{inb, outb};
use super::pio::{Port, COM1};
use crate::drivers::serial::Registers;
use crate::log;

/// COM1's register `n`.
const fn reg(n: u16) -> Port {
    COM1.port(n)
}

/// Line status register bits: a received byte waits, and the transmitter
/// holding register is empty.
const LSR: Port = reg(5);
const LSR_DATA_READY: u8 = 0x01;
const LSR_THR_EMPTY: u8 = 0x20;

/// Bytes the transmitter takes once [`tx_ready`] says so: in FIFO mode
/// `LSR.THRE` reads set only when the 16-byte FIFO is empty (PC16550D data
/// sheet, FIFO mode; [`init`] enables the FIFO).
pub const TX_BURST: usize = 16;

/// Program the 16550 and answer whether it is there: hardware with no SuperIO
/// reads 0xFF on every access, indistinguishable from a ready UART, so a
/// loopback probe latches the answer once.
pub fn init(_: &mut Registers, _rsdp_addr: u64) -> bool {
    // SAFETY: `outb`/`inb` require the caller to own the port and the byte;
    // the caller holds the registers, every port here is COM1's register `n` for `n`
    // in 0..=4, inside COM1's own register block, and the writes are the
    // 16550's documented init sequence.
    // Order matters: DLAB must precede the divisor writes and loopback mode
    // must precede the probe, or the sequence misprograms the chip.
    let loopback = unsafe {
        outb(reg(1), 0x00); // Disable all interrupts
        outb(reg(3), 0x80); // Enable DLAB (set baud rate divisor)
        outb(reg(0), 0x03); // Set divisor to 3 (lo byte) 38400 baud
        outb(reg(1), 0x00); //                  (hi byte)
        outb(reg(3), 0x03); // 8 bits, no parity, one stop bit
        outb(reg(2), 0xC7); // Enable FIFO, clear them, with 14-byte threshold
        outb(reg(4), 0x0B); // IRQs enabled, RTS/DSR set
        outb(reg(4), 0x1E); // Set in loopback mode, test the serial chip
        outb(reg(0), 0xAE); // Test serial chip (send byte 0xAE and check if serial returns same byte)
        let seen = inb(reg(0));
        outb(reg(4), 0x0F); // Normal operation mode
        seen
    };
    // Logs the raw byte, not just the verdict: distinguishes "no SuperIO"
    // (0xFF) from a wrong response and a right chip at the wrong port.
    log!(
        "serial: 16550 loopback read {:#04x} ({})",
        loopback,
        if loopback == 0xAE { "present" } else { "absent or wrong port" }
    );
    loopback == 0xAE
}

/// Whether a received byte waits.
pub fn rx_ready(_: &mut Registers) -> bool {
    inb(LSR) & LSR_DATA_READY != 0
}

/// The received byte; only after [`rx_ready`] said one waits.
pub fn read_byte(_: &mut Registers) -> u8 {
    inb(reg(0))
}

/// Whether the transmitter will take a byte.
pub fn tx_ready(_: &mut Registers) -> bool {
    inb(LSR) & LSR_THR_EMPTY != 0
}

/// Put one byte in the transmitter; only after [`tx_ready`], or the byte may be lost.
pub fn write_byte(_: &mut Registers, byte: u8) {
    // SAFETY: `outb` requires ownership of the port and the byte; the caller
    // holds the registers, register 0 is COM1's own data register, and the byte is
    // console output only.
    unsafe { outb(reg(0), byte) };
}
