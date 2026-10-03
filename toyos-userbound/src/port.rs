//! Which I/O ports the CPU a process runs on opens to Ring 3, and which port
//! a refused `in` or `out` named.
//!
//! **A process reaches a port only through its CPU's I/O permission bitmap,
//! with IOPL left at 0** (Intel SDM Vol. 1 §19.5.2, AMD APM Vol. 2 §12.2.4):
//! the TSS carries one bit per port below [`IO_PORTS`], set unless the process
//! running there holds a grant naming that port, and every port at or past it
//! is past the TSS limit, which the processor refuses by itself.

/// Ports the bitmap names. A port the bitmap can open is therefore a `u8`.
pub const IO_PORTS: usize = 0x100;

/// The bitmap as the processor reads it at the end of a TSS: one bit per port,
/// set to refuse, then the all-ones byte past the last (the processor reads two
/// bytes for every check, and the second must refuse).
#[repr(C)]
pub struct IoBitmap {
    refused: [u8; IO_PORTS / 8],
    end: u8,
}

const _: () = assert!(size_of::<IoBitmap>() == IO_PORTS / 8 + 1 && align_of::<IoBitmap>() == 1);

impl IoBitmap {
    /// Every port refused.
    pub const fn refusing() -> Self {
        Self { refused: [0xFF; IO_PORTS / 8], end: 0xFF }
    }

    /// Whether Ring 3 may access `port`.
    pub const fn opens(&self, port: u16) -> bool {
        (port as usize) < IO_PORTS && self.refused[port as usize / 8] & (1 << (port % 8)) == 0
    }

    fn set(&mut self, port: u8, open: bool) {
        let (at, bit) = (&mut self.refused[port as usize / 8], 1u8 << (port % 8));
        *at = if open { *at & !bit } else { *at | bit };
    }

    /// Open each row's ports if the process switching in holds the row, and
    /// close them otherwise: `rows` is every grantable row's ports, with
    /// whether the incoming process holds it.
    pub fn switch_to<'a>(&mut self, rows: impl IntoIterator<Item = (&'a [u8], bool)>) {
        for (ports, held) in rows {
            // A row's ports open and close together, so its first bit is its state.
            let Some(&first) = ports.first() else { continue };
            if self.opens(first as u16) == held {
                continue;
            }
            for &port in ports {
                self.set(port, held);
            }
        }
    }

    /// The first port of `access` this bitmap refuses, which is the port its
    /// #GP faulted on. `None` if every port in the span is open: the fault was
    /// not the port's, and its report must not guess one.
    pub fn refused(&self, access: PortAccess) -> Option<u16> {
        (0..access.bytes as u16).map(|i| access.port.wrapping_add(i)).find(|&port| !self.opens(port))
    }
}

/// An `in` or `out` as decoded from the bytes at a faulting instruction.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PortAccess {
    pub out: bool,
    pub port: u16,
    /// How many ports from `port` on the access spans, every one of which the
    /// bitmap must open.
    pub bytes: u8,
}

impl core::fmt::Display for PortAccess {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let (verb, way) = if self.out { ("out", "to") } else { ("in", "from") };
        write!(f, "{verb} of {} byte(s) {way} port {:#06x}", self.bytes, self.port)
    }
}

/// The port access `code` begins with, or `None` for any other instruction.
/// What a Ring 3 #GP names, since its error code (0) says nothing; `dx` is
/// what the forms that take their port from DX read.
///
/// Up to two prefixes of those an `in`/`out` can carry: operand size (which
/// halves a 4-byte access), a REP for the string forms, and REX.
pub const fn port_access(code: [u8; 4], dx: u16) -> Option<PortAccess> {
    const fn prefix(byte: u8) -> bool {
        matches!(byte, 0x66 | 0xF2 | 0xF3 | 0x40..=0x4F)
    }
    // Destructured rather than indexed: the crash report calls this, and
    // nothing on that path may panic.
    let (half, op, imm) = match code {
        [a, b, op, imm] if prefix(a) && prefix(b) => (a == 0x66 || b == 0x66, op, imm),
        [a, op, imm, _] if prefix(a) => (a == 0x66, op, imm),
        [op, imm, _, _] => (false, op, imm),
    };
    let wide = if half { 2 } else { 4 };
    let (out, port, bytes) = match op {
        0xE4 => (false, imm as u16, 1),
        0xE5 => (false, imm as u16, wide),
        0xE6 => (true, imm as u16, 1),
        0xE7 => (true, imm as u16, wide),
        0xEC | 0x6C => (false, dx, 1),
        0xED | 0x6D => (false, dx, wide),
        0xEE | 0x6E => (true, dx, 1),
        0xEF | 0x6F => (true, dx, wide),
        _ => return None,
    };
    Some(PortAccess { out, port, bytes })
}

const _: () = {
    const fn is(access: Option<PortAccess>, out: bool, port: u16, bytes: u8) -> bool {
        match access {
            Some(a) => a.out == out && a.port == port && a.bytes == bytes,
            None => false,
        }
    }
    // `in al, 0x61`, `out 0x64, al`, `in eax, dx`, `in ax, dx` and `rep outsb`.
    assert!(is(port_access([0xE4, 0x61, 0, 0], 0), false, 0x61, 1));
    assert!(is(port_access([0xE6, 0x64, 0, 0], 0), true, 0x64, 1));
    assert!(is(port_access([0xED, 0, 0, 0], 0x60), false, 0x60, 4));
    assert!(is(port_access([0x66, 0xED, 0, 0], 0x60), false, 0x60, 2));
    assert!(is(port_access([0xF3, 0x6E, 0, 0], 0x3F8), true, 0x3F8, 1));
    // `mov eax, 0x61` (B8) and `hlt` are no port access.
    assert!(port_access([0xB8, 0x61, 0, 0], 0).is_none());
    assert!(port_access([0xF4, 0, 0, 0], 0).is_none());
};

#[cfg(test)]
mod tests {
    extern crate std;

    use std::vec::Vec;

    use super::*;

    /// The i8042's row, and a second one beside it.
    const I8042: &[u8] = &[0x60, 0x64];
    const OTHER: &[u8] = &[0x70, 0x71];

    fn open_ports(bitmap: &IoBitmap) -> Vec<u16> {
        (0..=u16::MAX).filter(|&port| bitmap.opens(port)).collect()
    }

    #[test]
    fn a_fresh_bitmap_refuses_every_port_and_ends_in_ones() {
        let bitmap = IoBitmap::refusing();
        assert_eq!(open_ports(&bitmap), [] as [u16; 0]);
        assert_eq!(bitmap.end, 0xFF);
    }

    #[test]
    fn a_held_row_opens_its_ports_and_no_other() {
        let mut bitmap = IoBitmap::refusing();
        bitmap.switch_to([(I8042, true), (OTHER, false)]);
        assert_eq!(open_ports(&bitmap), [0x60, 0x64], "a port beside a granted one opened with it");
        // The byte the processor reads past the bitmap still refuses.
        assert_eq!(bitmap.end, 0xFF);
    }

    #[test]
    fn a_switch_to_a_process_holding_nothing_closes_the_row() {
        let mut bitmap = IoBitmap::refusing();
        bitmap.switch_to([(I8042, true), (OTHER, true)]);
        assert_eq!(open_ports(&bitmap), [0x60, 0x64, 0x70, 0x71]);
        bitmap.switch_to([(I8042, false), (OTHER, true)]);
        assert_eq!(open_ports(&bitmap), [0x70, 0x71], "the next process kept its predecessor's ports");
        bitmap.switch_to([(I8042, false), (OTHER, false)]);
        assert_eq!(open_ports(&bitmap), [] as [u16; 0]);
        bitmap.switch_to([(I8042, true), (OTHER, false)]);
        assert_eq!(open_ports(&bitmap), [0x60, 0x64], "the holder's return did not open its row again");
    }

    #[test]
    fn the_last_port_of_the_bitmap_opens_without_touching_the_end_byte() {
        let mut bitmap = IoBitmap::refusing();
        bitmap.switch_to([(&[0xFF][..], true)]);
        assert_eq!(open_ports(&bitmap), [0xFF]);
        assert_eq!(bitmap.end, 0xFF);
    }

    #[test]
    fn a_refused_access_names_the_first_port_it_may_not_touch() {
        let mut bitmap = IoBitmap::refusing();
        bitmap.switch_to([(I8042, true), (&[0xFF][..], true)]);
        let access = |port, bytes| PortAccess { out: false, port, bytes };
        // A granted port, alone: the fault is not the port's.
        assert_eq!(bitmap.refused(access(0x60, 1)), None);
        assert_eq!(bitmap.refused(access(0x64, 1)), None);
        // One past the grant, and a wider access that runs into it.
        assert_eq!(bitmap.refused(access(0x61, 1)), Some(0x61));
        assert_eq!(bitmap.refused(access(0x60, 2)), Some(0x61));
        assert_eq!(bitmap.refused(access(0x60, 4)), Some(0x61));
        // Past the bitmap is past the TSS limit, and the span wraps as ports do.
        assert_eq!(bitmap.refused(access(0xFF, 2)), Some(0x100));
        assert_eq!(bitmap.refused(access(0x3F8, 1)), Some(0x3F8));
        assert_eq!(bitmap.refused(access(0xFFFF, 2)), Some(0xFFFF));
    }
}
