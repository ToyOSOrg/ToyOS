//! A PCI-to-PCI bridge's forwarded memory windows (PCI-to-PCI Bridge
//! Architecture Specification §3.2.5.6 to §3.2.5.8).
//!
//! **What a bridge forwards, nothing above it may hand out.** An address inside
//! a bridge's window is routed to that bridge's secondary bus and answered by
//! whatever is on it — or by nothing, which reads as ones and is not
//! distinguishable from unrouted space. So a module placing a window has to know
//! these ranges before it can call any address free, and they are readable from
//! config space alone: no interpreter, no table.
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
    forwarded(pair, 0, 0)
}

/// Whether a prefetchable window's registers name a 64-bit range, in which case
/// the upper dwords at 0x28 and 0x2C carry the rest of it.
fn prefetch_is_64_bit(pair: u32) -> bool {
    pair & 0xF == 1
}

/// Byte offsets of the two dwords that carry the rest of a 64-bit prefetchable
/// window.
pub const PREFETCH_BASE_UPPER: u64 = 0x28;
pub const PREFETCH_LIMIT_UPPER: u64 = 0x2C;

/// The whole range a prefetchable window forwards, or `None` where it forwards
/// nothing.
///
/// **A 64-bit window's upper dwords are half its address**: read without them
/// it is a megabyte of the low space no bridge forwards, and the range above
/// 4 GiB it does forward is called free.
pub fn prefetch(pair: u32, base_upper: u32, limit_upper: u32) -> Option<Window> {
    // A 32-bit window's upper dwords are hardwired to zero and say nothing; a
    // machine that answers otherwise must not move the window over it.
    if !prefetch_is_64_bit(pair) {
        return window(pair);
    }
    forwarded(pair, base_upper, limit_upper)
}

fn forwarded(pair: u32, base_upper: u32, limit_upper: u32) -> Option<Window> {
    // The twelve bits at 15:4 of each half *are* address bits 31:20, so each
    // half moves left by sixteen and not by the four its own field is offset by.
    let base = (u64::from(base_upper) << 32) | (u64::from(pair & 0xFFF0) << 16);
    let limit = (u64::from(limit_upper) << 32) | (u64::from((pair >> 16) & 0xFFF0) << 16);
    // A base above its limit is the encoding for a bridge that forwards
    // nothing, and it is what firmware writes into a window it did not need —
    // read as a range it would be `0x00100000..0x0`, which wraps.
    if base > limit {
        return None;
    }
    // The limit names the last megabyte, not the first free one. Saturating,
    // because the one byte it drops is in no run: `placement::free_runs` stops
    // below `u64::MAX`.
    Some(Window { start: base, end: limit.saturating_add(GRANULE) })
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

    /// **A 64-bit prefetchable window is its upper dwords too**, from a range
    /// wholly above 4 GiB to one that crosses it; a 32-bit one is not moved by
    /// upper dwords a machine answers anything in.
    #[test]
    fn a_64_bit_prefetchable_window_is_its_upper_dwords_too() {
        assert_eq!(
            prefetch(0xbc31_bc21, 0x60, 0x60),
            Some(Window { start: 0x60_bc20_0000, end: 0x60_bc40_0000 })
        );
        assert_eq!(
            prefetch(0xfff1_c001, 0, 1),
            Some(Window { start: 0xc000_0000, end: 0x2_0000_0000 })
        );
        assert_eq!(
            prefetch(0xbc31_bc21, 0, 0),
            Some(Window { start: 0xbc20_0000, end: 0xbc40_0000 })
        );
        // Disabled is decided on the whole address, not on its low half.
        assert_eq!(prefetch(0xbc31_bc21, 0x61, 0x60), None);
        assert_eq!(
            prefetch(0xfff1_fff1, u32::MAX, u32::MAX),
            Some(Window { start: 0xffff_ffff_fff0_0000, end: u64::MAX })
        );
        assert_eq!(
            prefetch(0xbc30_bc20, 0x60, 0x60),
            Some(Window { start: 0xbc20_0000, end: 0xbc40_0000 })
        );
        assert_eq!(prefetch(0x0000_0010, 0, 0), None);
    }
}
