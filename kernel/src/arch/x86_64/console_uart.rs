//! The console UART: the 16550 at COM1's I/O ports, where every PC this
//! kernel boots puts one if it has one.

use super::cpu::{inb, outb};
use crate::log;

const PORT: u16 = 0x3f8; // COM1

/// Line status register bits: a received byte waits, and the transmitter
/// holding register is empty.
const LSR: u16 = PORT + 5;
const LSR_DATA_READY: u8 = 0x01;
const LSR_THR_EMPTY: u8 = 0x20;

/// Program the 16550 and answer whether it is there: hardware with no SuperIO
/// reads 0xFF on every access, indistinguishable from a ready UART, so a
/// loopback probe latches the answer once.
// Every register is `PORT + n`; the identity op keeps that pattern uniform
// across all eight lines instead of special-casing the data register.
#[allow(clippy::identity_op)]
pub fn init(_rsdp_addr: u64) -> bool {
    // SAFETY: `outb`/`inb` require the caller to own the port and the byte;
    // every port here is `PORT + n` for `n` in 0..=4, inside COM1's own
    // register block, and the writes are the 16550's documented init sequence.
    // Order matters: DLAB must precede the divisor writes and loopback mode
    // must precede the probe, or the sequence misprograms the chip.
    let loopback = unsafe {
        outb(PORT + 1, 0x00); // Disable all interrupts
        outb(PORT + 3, 0x80); // Enable DLAB (set baud rate divisor)
        outb(PORT + 0, 0x03); // Set divisor to 3 (lo byte) 38400 baud
        outb(PORT + 1, 0x00); //                  (hi byte)
        outb(PORT + 3, 0x03); // 8 bits, no parity, one stop bit
        outb(PORT + 2, 0xC7); // Enable FIFO, clear them, with 14-byte threshold
        outb(PORT + 4, 0x0B); // IRQs enabled, RTS/DSR set
        outb(PORT + 4, 0x1E); // Set in loopback mode, test the serial chip
        outb(PORT + 0, 0xAE); // Test serial chip (send byte 0xAE and check if serial returns same byte)
        let seen = inb(PORT + 0);
        outb(PORT + 4, 0x0F); // Normal operation mode
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
pub fn rx_ready() -> bool {
    inb(LSR) & LSR_DATA_READY != 0
}

/// The received byte; only after [`rx_ready`] said one waits.
pub fn read_byte() -> u8 {
    inb(PORT)
}

/// Whether the transmitter will take a byte.
pub fn tx_ready() -> bool {
    inb(LSR) & LSR_THR_EMPTY != 0
}

/// Put one byte in the transmitter; only after [`tx_ready`], or the byte may be lost.
pub fn write_byte(byte: u8) {
    // SAFETY: `outb` requires ownership of the port and the byte; `PORT` is
    // COM1's own data register, and the byte is console output only.
    unsafe { outb(PORT, byte) };
}
