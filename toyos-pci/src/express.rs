//! The PCI Express capability structure, and the two things this kernel does
//! through it: put a function back into its reset state, and put back the
//! Device Control words that reset returned to their defaults (PCIe base spec
//! 6.0 §7.5.3.3, §7.5.3.4, §7.5.3.16 and §6.6.2).
//!
//! **A function handed to a process comes back in whatever state that process
//! left it.** Its queues are still programmed with device addresses of a domain
//! that no longer maps them, and its own status register still says a driver is
//! attached. The only reset that means the same thing on every function is the
//! one the function itself implements — and QEMU's virtio functions implement
//! none, so `pcidev` does not rest on this: what makes a re-claim safe there is
//! that bus mastering starts on the first grant. This is the belt, taken where
//! a function offers it.

/// This capability's id in a function's capability list.
pub const CAP_ID: u8 = 0x10;

/// Byte offsets from the capability header.
pub const CAPABILITIES: u64 = 0x02;
pub const DEVICE_CAPABILITIES: u64 = 0x04;
pub const DEVICE_CONTROL: u64 = 0x08;
/// Device Control 2 (§7.5.3.16), which only a version 2 structure carries.
pub const DEVICE_CONTROL_2: u64 = 0x28;

/// PCI Express Capabilities bits 3:0, the structure's version (§7.5.3.2).
const VERSION: u16 = 0xF;

/// Device Capabilities bit 28, "Function Level Reset Capability" (§7.5.3.3).
const FLR_CAPABLE: u32 = 1 << 28;

/// Device Control bit 15, "Initiate Function Level Reset" (§7.5.3.4). Write-only
/// and self-clearing: it always reads zero.
const INITIATE_FLR: u16 = 1 << 15;

/// §6.6.2: after initiating the reset, software waits at least 100 ms before
/// it may access the function again.
pub const SETTLE_NANOS: u64 = 100_000_000;

/// Whether this function implements the reset, from its Device Capabilities.
pub const fn resets(device_capabilities: u32) -> bool {
    device_capabilities & FLR_CAPABLE != 0
}

/// The Device Control word that starts the reset.
///
/// Every other bit of the register is preserved: the reset clears them itself,
/// and a read-modify-write is what the register's own definition asks for —
/// this is the one bit that is not sticky.
pub const fn initiate(device_control: u16) -> u16 {
    device_control | INITIATE_FLR
}

/// Whether the structure is long enough to hold [`DEVICE_CONTROL_2`]: a
/// version 1 one ends before it, and what is there belongs to something else.
pub const fn has_control_2(capabilities: u16) -> bool {
    capabilities & VERSION >= 2
}

/// The Device Control word that puts back what a reset returned to its
/// default — Max_Payload_Size above all, which has to agree with the link
/// partner's (§7.5.3.4) — without starting a second reset.
pub const fn restored(device_control: u16) -> u16 {
    device_control & !INITIATE_FLR
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bit 28 and nothing else. The neighbours are Role-Based Error Reporting
    /// (bit 15) and the Captured Slot Power values (25:18), so an off-by-one
    /// here reads a power limit as a reset capability.
    #[test]
    fn only_bit_twenty_eight_says_a_function_resets() {
        assert!(resets(1 << 28));
        assert!(resets(u32::MAX));
        assert!(!resets(0));
        assert!(!resets(!(1u32 << 28)));
        for bit in 0..32 {
            assert_eq!(resets(1 << bit), bit == 28, "bit {bit}");
        }
    }

    /// The initiate bit is set and every other bit of Device Control survives:
    /// the register carries the function's error reporting, its max payload
    /// size and its relaxed-ordering enable, and dropping those on the way to a
    /// reset would reconfigure the link.
    #[test]
    fn initiating_a_reset_preserves_the_rest_of_device_control() {
        assert_eq!(initiate(0), 1 << 15);
        assert_eq!(initiate(0x0000), INITIATE_FLR);
        assert_eq!(initiate(0x5F7F), 0x5F7F | INITIATE_FLR);
        assert_eq!(initiate(u16::MAX), u16::MAX);
    }

    /// A restored word never carries the initiate bit, whatever was read, and
    /// keeps every other bit.
    #[test]
    fn restoring_device_control_never_starts_a_reset() {
        for word in [0u16, 0x5F7F, u16::MAX, INITIATE_FLR, 0x2810] {
            assert_eq!(restored(word) & INITIATE_FLR, 0, "{word:#06x}");
            assert_eq!(restored(word) | INITIATE_FLR, word | INITIATE_FLR, "{word:#06x}");
        }
        assert_eq!(restored(initiate(0x2810)), 0x2810);
    }

    /// Version 2 and up hold Device Control 2; version 1 does not, and the
    /// device/port type in bits 7:4 is not read as a version.
    #[test]
    fn only_a_version_two_structure_holds_device_control_2() {
        assert!(!has_control_2(0x0001));
        assert!(has_control_2(0x0002));
        assert!(has_control_2(0x0042));
        assert!(!has_control_2(0x00F1));
        assert!(!has_control_2(0x0000));
    }
}
