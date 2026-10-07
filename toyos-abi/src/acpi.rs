//! What a claim on the machine's ACPI fixed hardware hands the process that
//! serves its events.
//!
//! The kernel puts the machine in ACPI mode when it mints the claim and back
//! in the mode its firmware handed over when the claim goes; the holder gets
//! the register blocks the FADT and the ECDT name, as ports it may `in` and
//! `out`, and the SCI as records on its own claim handle.
//!
//! **The SCI is a level line, masked by the kernel each time it is taken.** The
//! holder clears the status bits behind it and then acknowledges the claim
//! ([`ACK`]); a line acknowledged with a status bit still set is taken again.

/// A run of ports a register block occupies; `len` 0 is no block.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Block {
    pub port: u16,
    pub len: u16,
}

impl Block {
    pub const NONE: Self = Self { port: 0, len: 0 };

    /// A status-and-enable block's enable half (ACPI 6.5 §4.8.1): the status
    /// registers are its first half, its enable registers the second.
    pub const fn enable(self) -> u16 {
        self.port + self.len / 2
    }
}

/// The FADT's power button is the fixed-hardware one, `PWRBTN_STS` and
/// `PWRBTN_EN` in the PM1 event block.
pub const FIXED_POWER_BUTTON: u16 = 1 << 0;

/// The claim's description.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AcpiInfo {
    /// The PM1a event block, its status half then its enable half.
    pub pm1_event: Block,
    /// The GPE0 block, its status half then its enable half.
    pub gpe0: Block,
    /// The embedded controller's command/status and data ports, as the ECDT
    /// names them; `len` 0 where the machine named none.
    pub ec_command: Block,
    pub ec_data: Block,
    /// The GPE the embedded controller raises, inside [`Self::gpe0`].
    pub ec_gpe: u16,
    pub flags: u16,
}

/// Every byte belongs to a field: this crosses the boundary through
/// `as_bytes`, so a gap would publish whatever the kernel stack held.
const _: () = assert!(core::mem::size_of::<AcpiInfo>() == 4 * 4 + 2 + 2);

impl AcpiInfo {
    pub fn as_bytes(&self) -> &[u8] {
        // SAFETY: `self` is a valid `&Self`, and the const assert above proves
        // the `repr(C)` layout of `u16`s has no padding, so every byte the
        // slice exposes is an initialized field.
        unsafe { core::slice::from_raw_parts(self as *const Self as *const u8, core::mem::size_of::<Self>()) }
    }

    pub fn has_ec(&self) -> bool {
        self.ec_command.len != 0
    }
}

/// The word a holder writes to its claim to have the SCI unmasked.
pub const ACK: u32 = 1;
