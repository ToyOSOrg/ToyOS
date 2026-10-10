//! HID over I2C (Microsoft's protocol specification, version 1.00), the
//! bytes only: what the device's HID descriptor says, where a report
//! descriptor's mouse collection puts its buttons and motion, and what one
//! input-register read of that collection moved.
//!
//! Every byte here is one a device chose. A descriptor or report that does
//! not read as its layout is refused by name, and no input panics: every
//! function is total over every byte string. Nothing here touches a device,
//! allocates, or holds a lock.

#![no_std]
#![forbid(unsafe_code)]

pub mod report;

/// The HID descriptor's length (§5.1.1): its first field, and what a read of
/// the descriptor register asks for.
pub const HID_DESCRIPTOR_LEN: usize = 30;

/// What the device's HID descriptor (§5.1.1) says, the fields this driver
/// uses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct HidDescriptor {
    pub report_descriptor_len: u16,
    pub report_descriptor_register: u16,
    pub input_register: u16,
    pub max_input_len: u16,
    pub command_register: u16,
    pub data_register: u16,
    pub vendor: u16,
    pub product: u16,
    pub version: u16,
}

/// Why a HID descriptor was not believed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DescriptorRefused {
    /// Fewer bytes than the descriptor's 30.
    Short(usize),
    /// `wHIDDescLength` is not 30.
    Length(u16),
    /// `bcdVersion` is not 1.00.
    Version(u16),
    /// An input length that cannot hold its own two-byte length field.
    MaxInput(u16),
}

impl HidDescriptor {
    pub fn parse(bytes: &[u8]) -> Result<Self, DescriptorRefused> {
        let Some(b) = bytes.get(..HID_DESCRIPTOR_LEN) else {
            return Err(DescriptorRefused::Short(bytes.len()));
        };
        let w = |at: usize| u16::from_le_bytes([b[at], b[at + 1]]);
        if w(0) as usize != HID_DESCRIPTOR_LEN {
            return Err(DescriptorRefused::Length(w(0)));
        }
        if w(2) != 0x0100 {
            return Err(DescriptorRefused::Version(w(2)));
        }
        if w(10) < 2 {
            return Err(DescriptorRefused::MaxInput(w(10)));
        }
        Ok(Self {
            report_descriptor_len: w(4),
            report_descriptor_register: w(6),
            input_register: w(8),
            max_input_len: w(10),
            command_register: w(16),
            data_register: w(18),
            vendor: w(20),
            product: w(22),
            version: w(24),
        })
    }
}

/// What one read of the input register (§6.1.1) delivered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Input<'a> {
    /// A length of zero: no report, or the reset's answer (§7.2.1).
    Nothing,
    /// The report, its report id first where the descriptor declares ids.
    Report(&'a [u8]),
    /// A length below the two bytes of the field itself, or past the read.
    Refused { len: u16 },
}

/// The input register's read, split at its two-byte length field.
pub fn input(read: &[u8]) -> Input<'_> {
    let [lo, hi, ..] = *read else { return Input::Refused { len: 0 } };
    let len = u16::from_le_bytes([lo, hi]);
    match len {
        0 => Input::Nothing,
        2.. if len as usize <= read.len() => Input::Report(&read[2..len as usize]),
        _ => Input::Refused { len },
    }
}

/// A command (§7.2) to the command register: the register's address, then
/// the command word, low byte first.
pub const fn command(command_register: u16, opcode: u8, low_nibble: u8) -> [u8; 4] {
    let r = command_register.to_le_bytes();
    [r[0], r[1], low_nibble & 0xF, opcode & 0xF]
}

/// RESET (§7.2.1).
pub const RESET: u8 = 0x1;
/// SET_POWER (§7.2.8), whose low nibble is the power state: 0 is ON.
pub const SET_POWER: u8 = 0x8;
pub const POWER_ON: u8 = 0x0;

#[cfg(test)]
mod tests;
