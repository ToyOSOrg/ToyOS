//! The firmware's error tables (§12.2), read from device memory dword by dword once it asserts:
//! the LMAC's at the alive notification's LMAC error-table address and the UMAC's at its UMAC
//! one. A first dword of 0 is an empty table.

/// The LMAC table's first 24 dwords of its 38, through the last host command header (§12.2).
pub const LMAC_DWORDS: usize = 24;
/// The UMAC table's 15 dwords.
pub const UMAC_DWORDS: usize = 15;

/// What the LMAC table names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lmac {
    pub valid: u32,
    pub error: u32,
    pub hardware_version: u32,
}

/// What the UMAC table names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Umac {
    pub valid: u32,
    pub error: u32,
    /// For a refused command: its first dword, the length it should have, the length it had.
    pub data: [u32; 3],
    pub major: u32,
    pub minor: u32,
    /// The last host command header.
    pub last_command: u32,
}

impl Lmac {
    /// `None` for an empty table.
    pub fn read(dwords: &[u32; LMAC_DWORDS]) -> Option<Lmac> {
        (dwords[0] != 0).then(|| Lmac { valid: dwords[0], error: dwords[1], hardware_version: dwords[18] })
    }
}

impl Umac {
    /// `None` for an empty table.
    pub fn read(dwords: &[u32; UMAC_DWORDS]) -> Option<Umac> {
        (dwords[0] != 0).then(|| Umac {
            valid: dwords[0],
            error: dwords[1],
            data: [dwords[6], dwords[7], dwords[8]],
            major: dwords[9],
            minor: dwords[10],
            last_command: dwords[13],
        })
    }
}

/// An error identifier's name, bits 31:28 masked off.
pub fn name(error: u32) -> &'static str {
    match error & 0x0fff_ffff {
        0x34 => "watchdog NMI",
        0x35 => "system assert",
        0x37 => "firmware version mismatch",
        0x38 | 0x39 => "bad command",
        0x3c => "data action point NMI",
        0x3d => "fatal error",
        0x46 => "TRM hardware error NMI",
        0x4c => "TRM NMI",
        0x54 => "breakpoint NMI",
        0x5c => "receive FIFO full watchdog",
        0x64 => "receive FIFO full without buffers watchdog",
        0x66 => "host NMI",
        0x70 => "LMAC fatal",
        0x71 => "UMAC fatal",
        0x73 => "other LMAC fatal",
        0x7c => "action point NMI",
        0x84 => "unknown NMI",
        0x86 => "instruction action point NMI",
        _ => "advanced system assert",
    }
}
