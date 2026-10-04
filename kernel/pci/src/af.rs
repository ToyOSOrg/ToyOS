//! The Advanced Features capability, and the function level reset it carries
//! on a conventional PCI function (PCI Advanced Capabilities for Conventional
//! PCI ECN, 2006; the PCH's integrated functions publish it where a PCI Express
//! function would publish the reset in its Express capability).

/// This capability's id in a function's capability list.
pub const CAP_ID: u8 = 0x13;

/// Byte offsets from the capability header.
pub const AF_CAPABILITIES: u64 = 0x03;
pub const AF_CONTROL: u64 = 0x04;
pub const AF_STATUS: u64 = 0x05;

/// AF Capabilities bit 1: the function implements FLR. Bit 0 says it reports
/// Transactions Pending.
const FLR_CAPABLE: u8 = 1 << 1;
const TP_CAPABLE: u8 = 1 << 0;

/// AF Control bit 0: initiate FLR. Write-only and self-clearing.
pub const INITIATE_FLR: u8 = 1 << 0;

/// AF Status bit 0: the function has transactions it has issued and not seen
/// completed.
const TRANSACTIONS_PENDING: u8 = 1 << 0;

/// The ECN's own wait after initiating the reset before the function may be
/// touched again: 100 ms, as for the Express reset.
pub const SETTLE_NANOS: u64 = 100_000_000;

/// Whether the function implements the reset, from AF Capabilities. **Both
/// bits**: the ECN makes a function that implements FLR implement the
/// Transactions Pending bit with it, and a reset whose outstanding writes
/// cannot be asked about is not the one described here.
pub const fn resets(af_capabilities: u8) -> bool {
    af_capabilities & (FLR_CAPABLE | TP_CAPABLE) == FLR_CAPABLE | TP_CAPABLE
}

/// Whether the function still has transactions outstanding.
pub const fn pending(af_status: u8) -> bool {
    af_status & TRANSACTIONS_PENDING != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_function_resets_only_with_both_bits() {
        assert!(resets(0b11));
        assert!(resets(0xff));
        for without in [0b00, 0b01, 0b10, 0xfc] {
            assert!(!resets(without), "{without:#x}");
        }
    }

    #[test]
    fn pending_is_bit_zero_of_status() {
        assert!(pending(1));
        assert!(!pending(0xfe));
    }
}
