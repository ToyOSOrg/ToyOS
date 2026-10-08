//! Which I/O ports the CPU a process runs on opens to Ring 3, which ports no
//! grant may ever name, and which port a refused `in` or `out` named.
//!
//! **A process reaches a port only through its CPU's I/O permission bitmap,
//! with IOPL left at 0** (Intel SDM Vol. 1 §19.5.2, AMD APM Vol. 2 §12.2.4):
//! the TSS carries one bit per port of the whole 16-bit space, set unless the
//! process running there holds a grant naming that port.
//!
//! **A grant never reaches a port the kernel declared** ([`Reserved`]): every
//! port this kernel drives is declared once, by what drives it, and a grant
//! that names one is refused naming that holder.

/// Ports the bitmap names: every port there is.
pub const IO_PORTS: usize = 0x10000;

/// A run of consecutive ports: a register block, or what a grant or a
/// declaration names. Never empty and never past the last port.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Ports {
    first: u16,
    count: u16,
}

impl Ports {
    /// `None` for no ports, or a run past port `0xFFFF`.
    pub const fn new(first: u16, count: u16) -> Option<Self> {
        if count == 0 || first as usize + count as usize > IO_PORTS {
            return None;
        }
        Some(Self { first, count })
    }

    pub const fn one(port: u16) -> Self {
        Self { first: port, count: 1 }
    }

    pub const fn first(self) -> u16 {
        self.first
    }

    pub const fn count(self) -> u16 {
        self.count
    }

    /// The port `offset` into the run, or `None` past its end.
    pub const fn at(self, offset: u16) -> Option<u16> {
        if offset < self.count { Some(self.first + offset) } else { None }
    }

    pub const fn overlaps(self, other: Self) -> bool {
        (self.first as u32) < other.first as u32 + other.count as u32
            && (other.first as u32) < self.first as u32 + self.count as u32
    }

    pub fn iter(self) -> impl Iterator<Item = u16> {
        (0..self.count).map(move |i| self.first + i)
    }
}

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
        self.refused[port as usize / 8] & (1 << (port % 8)) == 0
    }

    fn set(&mut self, port: u16, open: bool) {
        let (at, bit) = (&mut self.refused[port as usize / 8], 1u8 << (port % 8));
        *at = if open { *at & !bit } else { *at | bit };
    }

    /// Open each row's ports if the process switching in holds the row, and
    /// close them otherwise: `rows` is every grantable row's runs, with
    /// whether the incoming process holds it.
    pub fn switch_to<'a>(&mut self, rows: impl IntoIterator<Item = (&'a [Ports], bool)>) {
        for (runs, held) in rows {
            // A row's ports open and close together, so its first bit is its state.
            let Some(first) = runs.first() else { continue };
            if self.opens(first.first) == held {
                continue;
            }
            for port in runs.iter().flat_map(|run| run.iter()) {
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

/// What a declared run answers the `acpi` claim's holder, whose AML may name
/// any port ([`crate::firmware::port`]). Every declaration says, so none
/// passes by default.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mediated {
    /// Refused both ways.
    Kept,
    /// Read for the holder, never written.
    ReadOnly,
    /// Read and written for the holder.
    Open,
}

/// The ports no grant reaches, each run named by what holds it: the one
/// declaration of every port the kernel drives, read by whatever decides a
/// grant.
pub struct Reserved<const N: usize> {
    runs: [Option<(&'static str, Ports, Mediated)>; N],
}

/// Why a run was not declared.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Undeclared {
    /// It shares a port with the run this holder declared first.
    Clash(&'static str),
    /// Every slot is taken.
    Full,
}

impl<const N: usize> Default for Reserved<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Reserved<N> {
    pub const fn new() -> Self {
        Self { runs: [None; N] }
    }

    /// Reserve `ports` for `holder`, refused where another holder has one of them.
    pub fn declare(&mut self, holder: &'static str, ports: Ports, mediated: Mediated) -> Result<(), Undeclared> {
        if let Some(first) = self.holder(ports) {
            return Err(Undeclared::Clash(first));
        }
        let slot = self.runs.iter_mut().find(|slot| slot.is_none()).ok_or(Undeclared::Full)?;
        *slot = Some((holder, ports, mediated));
        Ok(())
    }

    /// Who holds a port of `ports`, if anyone does.
    pub fn holder(&self, ports: Ports) -> Option<&'static str> {
        self.runs.iter().flatten().find(|(_, held, _)| held.overlaps(ports)).map(|&(name, ..)| name)
    }

    /// What the run holding `port` answers a mediated access, if one does.
    pub fn mediated(&self, port: u16) -> Option<Mediated> {
        self.runs.iter().flatten().find(|(_, held, _)| held.overlaps(Ports::one(port))).map(|&(.., mediated)| mediated)
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

    fn run(first: u16, count: u16) -> Ports {
        Ports::new(first, count).expect("a run inside the port space")
    }

    /// The i8042's row, and a row of two runs high in the space, as a
    /// firmware's PM1 event block and its embedded controller's ports are.
    const I8042: &[Ports] = &[Ports::one(0x60), Ports::one(0x64)];
    fn high() -> [Ports; 2] {
        [run(0x1800, 4), run(0xFFFE, 2)]
    }

    fn open_ports(bitmap: &IoBitmap) -> Vec<u16> {
        (0..=u16::MAX).filter(|&port| bitmap.opens(port)).collect()
    }

    #[test]
    fn a_run_is_never_empty_and_never_past_the_last_port() {
        assert_eq!(Ports::new(0x60, 0), None);
        assert_eq!(Ports::new(0xFFFF, 2), None);
        assert_eq!(Ports::new(0xFFFF, 1).map(|p| p.iter().collect::<Vec<_>>()), Some(std::vec![0xFFFF]));
        assert_eq!(run(0x1800, 4).at(3), Some(0x1803));
        assert_eq!(run(0x1800, 4).at(4), None);
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
        bitmap.switch_to([(I8042, true), (&high()[..], false)]);
        assert_eq!(open_ports(&bitmap), [0x60, 0x64], "a port beside a granted one opened with it");
        // The byte the processor reads past the bitmap still refuses.
        assert_eq!(bitmap.end, 0xFF);
    }

    #[test]
    fn a_switch_to_a_process_holding_nothing_closes_the_row() {
        let mut bitmap = IoBitmap::refusing();
        let high = high();
        bitmap.switch_to([(I8042, true), (&high[..], true)]);
        assert_eq!(open_ports(&bitmap), [0x60, 0x64, 0x1800, 0x1801, 0x1802, 0x1803, 0xFFFE, 0xFFFF]);
        bitmap.switch_to([(I8042, false), (&high[..], true)]);
        assert_eq!(
            open_ports(&bitmap),
            [0x1800, 0x1801, 0x1802, 0x1803, 0xFFFE, 0xFFFF],
            "the next process kept its predecessor's ports"
        );
        bitmap.switch_to([(I8042, false), (&high[..], false)]);
        assert_eq!(open_ports(&bitmap), [] as [u16; 0]);
        bitmap.switch_to([(I8042, true), (&high[..], false)]);
        assert_eq!(open_ports(&bitmap), [0x60, 0x64], "the holder's return did not open its row again");
    }

    #[test]
    fn the_last_port_of_the_space_opens_without_touching_the_end_byte() {
        let mut bitmap = IoBitmap::refusing();
        bitmap.switch_to([(&[Ports::one(0xFFFF)][..], true)]);
        assert_eq!(open_ports(&bitmap), [0xFFFF]);
        assert_eq!(bitmap.end, 0xFF);
    }

    #[test]
    fn a_refused_access_names_the_first_port_it_may_not_touch() {
        let mut bitmap = IoBitmap::refusing();
        bitmap.switch_to([(I8042, true), (&[Ports::one(0xFF)][..], true)]);
        let access = |port, bytes| PortAccess { out: false, port, bytes };
        // A granted port, alone: the fault is not the port's.
        assert_eq!(bitmap.refused(access(0x60, 1)), None);
        assert_eq!(bitmap.refused(access(0x64, 1)), None);
        // One past the grant, and a wider access that runs into it.
        assert_eq!(bitmap.refused(access(0x61, 1)), Some(0x61));
        assert_eq!(bitmap.refused(access(0x60, 2)), Some(0x61));
        assert_eq!(bitmap.refused(access(0x60, 4)), Some(0x61));
        assert_eq!(bitmap.refused(access(0xFF, 2)), Some(0x100));
        assert_eq!(bitmap.refused(access(0x3F8, 1)), Some(0x3F8));
        // The span wraps as ports do.
        assert_eq!(bitmap.refused(access(0xFFFF, 2)), Some(0xFFFF));
    }

    #[test]
    fn a_declared_run_refuses_every_run_that_shares_a_port_with_it() {
        let mut reserved = Reserved::<4>::new();
        reserved.declare("the PM1a control block", run(0x1804, 2), Mediated::ReadOnly).expect("the first run");
        reserved.declare("SMI_CMD", Ports::one(0xB2), Mediated::Kept).expect("a disjoint run");
        assert_eq!(reserved.mediated(0x1805), Some(Mediated::ReadOnly));
        assert_eq!(reserved.mediated(0xB2), Some(Mediated::Kept));
        assert_eq!(reserved.mediated(0x1806), None);
        // Either end of the block, a run that covers it, and a run beside it.
        assert_eq!(reserved.holder(Ports::one(0x1804)), Some("the PM1a control block"));
        assert_eq!(reserved.holder(Ports::one(0x1805)), Some("the PM1a control block"));
        assert_eq!(reserved.holder(run(0x1800, 8)), Some("the PM1a control block"));
        assert_eq!(reserved.holder(run(0x1800, 4)), None);
        assert_eq!(reserved.holder(Ports::one(0x1806)), None);
        assert_eq!(reserved.holder(run(0xB0, 3)), Some("SMI_CMD"));
        // A second holder of a declared port is refused naming the first.
        assert_eq!(reserved.declare("the GPE0 block", run(0x1805, 1), Mediated::Open), Err(Undeclared::Clash("the PM1a control block")));
        assert_eq!(reserved.holder(run(0x1806, 0x10)), None, "a refused declaration reserved nothing");
    }

    #[test]
    fn a_full_declaration_refuses_one_more_run() {
        let mut reserved = Reserved::<1>::new();
        reserved.declare("COM1", run(0x3F8, 8), Mediated::Kept).expect("the one slot");
        assert_eq!(reserved.declare("the RTC", run(0x70, 2), Mediated::Kept), Err(Undeclared::Full));
    }
}
