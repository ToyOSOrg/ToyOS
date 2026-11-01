//! Which radio a function is, and whether its firmware file serves it (§1).

use crate::command::Band;
use crate::container::Firmware;

pub const INTEL: u16 = 0x8086;

/// A radio this driver serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    /// `8086:2723`, on the AMD bench.
    Ax200,
    /// `8086:2725`, on the T14.
    Ax210,
}

impl Part {
    /// The part a PCI function's identity names, if it is one.
    pub fn of(vendor: u16, device: u16) -> Option<Part> {
        match (vendor, device) {
            (INTEL, 0x2723) => Some(Part::Ax200),
            (INTEL, 0x2725) => Some(Part::Ax210),
            _ => None,
        }
    }

    /// The firmware file's name under `system/ax2xx/firmware/`.
    pub const fn firmware(self) -> &'static str {
        match self {
            Part::Ax200 => "iwlwifi-cc-a0-77.ucode",
            Part::Ax210 => "iwlwifi-ty-a0-gf-a0-89.ucode",
        }
    }

    /// The NVM profile's entries the part has (§10.5): the 2.4 and 5 GHz channels on the AX200,
    /// and the 6 GHz channels after them on the AX210. Entries past them say nothing.
    pub const fn profile_channels(self) -> usize {
        match self {
            Part::Ax200 => 51,
            Part::Ax210 => crate::notification::PROFILE_CHANNELS,
        }
    }

    /// The channel the NVM profile's entry `index` is for (§10.5): 2.4 GHz channels 1 to 14, 5 GHz
    /// channels 36 to 144 and 149 to 181 by fours, then on the AX210 6 GHz channels 1 to 233 by
    /// fours. `None` past the part's entries.
    pub fn profile_channel(self, index: usize) -> Option<(Band, u8)> {
        let n = |offset: usize, first: u8, step: u8| -> Option<u8> { u8::try_from(offset).ok()?.checked_mul(step)?.checked_add(first) };
        if index >= self.profile_channels() {
            return None;
        }
        match index {
            0..14 => Some((Band::Ghz2_4, n(index, 1, 1)?)),
            14..42 => Some((Band::Ghz5, n(index.checked_sub(14)?, 36, 4)?)),
            42..51 => Some((Band::Ghz5, n(index.checked_sub(42)?, 149, 4)?)),
            _ => Some((Band::Ghz6, n(index.checked_sub(51)?, 1, 4)?)),
        }
    }

    /// The platform-NVM file's, which only the AX210 is handed (§7.4).
    pub const fn pnvm(self) -> Option<&'static str> {
        match self {
            Part::Ax200 => None,
            Part::Ax210 => Some("iwlwifi-ty-a0-gf-a0.pnvm"),
        }
    }
}

/// A MAC type and an RF type: what a part reads as, and what a file is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hardware {
    pub mac: u16,
    pub rf: u16,
}

impl Hardware {
    /// The types a part reads as: bits 15:4 of the hardware revision register
    /// (BAR 0 + 0x028) and bits 23:12 of the RF identity register (+ 0x09c).
    pub fn read(hardware_revision: u32, rf_identity: u32) -> Hardware {
        Hardware { mac: twelve_bits(hardware_revision >> 4), rf: twelve_bits(rf_identity >> 12) }
    }
}

fn twelve_bits(word: u32) -> u16 {
    u16::try_from(word & 0xfff).unwrap_or(u16::MAX)
}

/// Why a part is not served.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotServed {
    /// The file handed over is for other hardware than the part's file.
    File { part: Part, file: Hardware },
    /// The part reads as other hardware than its file is for.
    Hardware { part: Part, read: Hardware },
}

impl Part {
    /// The hardware the part's own file is for (§1): what its hardware-type
    /// record names, and what the part must read as.
    pub const fn hardware(self) -> Hardware {
        match self {
            Part::Ax200 => Hardware { mac: 0x34, rf: 0x10a },
            Part::Ax210 => Hardware { mac: 0x42, rf: 0x10d },
        }
    }

    /// Whether `file` is this part's and the function reads as the hardware
    /// it is for.
    pub fn serve(self, read: Hardware, file: &Firmware<'_>) -> Result<(), NotServed> {
        if file.hardware != self.hardware() {
            return Err(NotServed::File { part: self, file: file.hardware });
        }
        match read == file.hardware {
            true => Ok(()),
            false => Err(NotServed::Hardware { part: self, read }),
        }
    }
}
