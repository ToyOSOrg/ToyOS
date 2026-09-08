//! A PCI-to-PCI bridge's forwarded memory windows (PCI-to-PCI Bridge
//! Architecture Specification §3.2.5.6-3.2.5.8).
//!
//! **What a bridge forwards, nothing above it may hand out.** An address inside
//! a bridge's window is routed to that bridge's secondary bus and answered by
//! whatever is on it — or by nothing, which reads as ones and is not
//! distinguishable from unrouted space. So a module placing a window below
//! 4 GiB has to know these ranges before it can call any address free, and they
//! are readable from config space alone: no interpreter, no table.
//!
//! The registers hold address bits 31:20 in their top twelve bits and hardwire
//! the rest, so every window is a whole number of megabytes and a *limit* names
//! the last byte rather than the first free one. A bridge forwarding nothing
//! writes a base above its limit, which is the encoding for "disabled" and the
//! one this decode has to get right: read literally it is a range that wraps.

/// Byte offsets in a Type 1 header.
pub const MEMORY_BASE: u64 = 0x20;
pub const PREFETCH_BASE: u64 = 0x24;

/// The Type 1 header, as `HEADER_TYPE` reports it with the multi-function bit
/// removed.
pub const HEADER_TYPE_BRIDGE: u8 = 1;

/// The granularity both windows are expressed in: bits 31:20.
const GRANULE: u64 = 1 << 20;

/// One forwarded range, `start..end`, `end` exclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub start: u64,
    pub end: u64,
}

/// The window a Memory Base/Limit pair describes, or `None` where the bridge
/// forwards nothing.
///
/// `pair` is the dword at [`MEMORY_BASE`] or [`PREFETCH_BASE`]: base in the low
/// half, limit in the high half. Only the top twelve bits of each half are the
/// address; the low four are the type field for a prefetchable window and are
/// reserved for a non-prefetchable one, and neither is part of the range.
pub fn window(pair: u32) -> Option<Window> {
    // The twelve bits at 15:4 of each half *are* address bits 31:20, so each
    // half moves left by sixteen and not by the four its own field is offset by.
    let base = u64::from(pair & 0xFFF0) << 16;
    let limit = u64::from((pair >> 16) & 0xFFF0) << 16;
    // A base above its limit is the encoding for a bridge that forwards
    // nothing, and it is what firmware writes into a window it did not need —
    // read as a range it would be `0x00100000..0x0`, which wraps.
    if base > limit {
        return None;
    }
    // The limit names the last megabyte, not the first free one.
    Some(Window { start: base, end: limit + GRANULE })
}

/// Whether a prefetchable window's registers name a 64-bit range, in which case
/// the upper dwords at 0x28 and 0x2C carry the rest of it.
///
/// Answered rather than decoded, because a module that hands out only 32-bit
/// space needs to know that a window it read the low half of may reach far
/// above what it can see — and treating that as a 32-bit range would call
/// addresses free that the bridge forwards.
pub fn prefetch_is_64_bit(pair: u32) -> bool {
    pair & 0xF == 1
}

/// Byte offsets of the two dwords that carry the rest of a 64-bit prefetchable
/// window.
pub const PREFETCH_BASE_UPPER: u64 = 0x28;
pub const PREFETCH_LIMIT_UPPER: u64 = 0x2C;

/// The part of a prefetchable window that lies below 4 GiB, or `None` where
/// none of it does.
///
/// **A survey of the low space may not count a window that is not in it.** A
/// 64-bit prefetchable window whose upper base is set begins above 4 GiB
/// entirely, and reading its low half as a range would call a megabyte of the
/// low space forwarded that no bridge forwards. Where the upper base is zero
/// the low half *is* the low part of the range, whatever the upper limit adds
/// above it.
pub fn prefetch_below_4g(pair: u32, base_upper: u32) -> Option<Window> {
    if prefetch_is_64_bit(pair) && base_upper != 0 {
        return None;
    }
    window(pair)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The registers hold bits 31:20 and hardwire the rest, so a window is
    /// whole megabytes and its limit names the last one.
    #[test]
    fn the_limit_names_the_last_megabyte_and_not_the_first_free_one() {
        // Limit in the high half, base in the low one. Base 0xbc200000 and
        // limit 0xbc2fffff: one megabyte forwarded.
        assert_eq!(window(0xbc20_bc20), Some(Window { start: 0xbc20_0000, end: 0xbc30_0000 }));
        // Base 0xbc200000, limit 0xbc3fffff: two.
        assert_eq!(window(0xbc30_bc20), Some(Window { start: 0xbc20_0000, end: 0xbc40_0000 }));
        // And the address really is bits 31:20 — a half read as if its field
        // offset were the shift would land a thousandth of the way up.
        assert_eq!(window(0x0000_0000), Some(Window { start: 0, end: 0x0010_0000 }));
    }

    /// **A bridge that forwards nothing must not read as a range.** Firmware
    /// writes a base above the limit for a window it did not need, and taken
    /// literally that is a range which wraps — and a free-space search over a
    /// wrapped range calls the whole of memory forwarded, or none of it.
    #[test]
    fn a_disabled_window_is_no_window() {
        // The canonical disabled encoding: base 0x00100000, limit 0x00000000.
        assert_eq!(window(0x0000_0010), None);
        // And the widest form of the same thing.
        assert_eq!(window(0x0000_fff0), None);
        // A bridge forwarding *everything* is the opposite and not the same
        // reading: base 0, limit 0xfff00000.
        assert_eq!(window(0xfff0_0000), Some(Window { start: 0, end: 0xfff0_0000 + GRANULE }));
        // Equal halves are one megabyte and not nothing.
        assert_eq!(window(0x0010_0010), Some(Window { start: 0x0010_0000, end: 0x0020_0000 }));
    }

    /// The low four bits of each half are the type field, never the address.
    /// Reading them as address bits moves a window by up to a megabyte.
    #[test]
    fn the_type_field_is_not_part_of_the_address() {
        let plain = window(0xbc30_bc20).expect("a forwarded window");
        let typed = window(0xbc3f_bc21).expect("the same window, prefetchable and 64-bit");
        assert_eq!(plain, typed);
        assert!(prefetch_is_64_bit(0xbc3f_bc21));
        assert!(!prefetch_is_64_bit(0xbc30_bc20));
    }

    /// A 64-bit prefetchable window that starts above 4 GiB is not a low range,
    /// and its low half is not one either.
    #[test]
    fn a_prefetchable_window_above_four_gigabytes_is_not_low_space() {
        assert_eq!(prefetch_below_4g(0xbc31_bc21, 0x60), None);
        assert_eq!(
            prefetch_below_4g(0xbc31_bc21, 0),
            Some(Window { start: 0xbc20_0000, end: 0xbc40_0000 })
        );
        // A 32-bit window's upper dwords are hardwired to zero and say nothing;
        // a machine that answers otherwise must not lose the window over it.
        assert_eq!(
            prefetch_below_4g(0xbc30_bc20, 0x60),
            Some(Window { start: 0xbc20_0000, end: 0xbc40_0000 })
        );
        assert_eq!(prefetch_below_4g(0x0000_0010, 0), None);
    }
}
