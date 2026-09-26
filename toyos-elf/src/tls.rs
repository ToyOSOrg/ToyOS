//! Where everything goes inside a thread's TLS allocation, in the layout each
//! machine's psABI names ([`Variant`]), with the DTV the kernel writes at the
//! front of the allocation:
//!
//! ```text
//! variant II (x86-64):  [DTV] [pad] [TLS data (.tdata + .tbss)] [TCB]
//!                                    ^ tls_start                 ^ thread pointer
//! variant I (AArch64):  [DTV] [pad] [TCB] [TLS data (.tdata + .tbss)]
//!                                    ^ thread pointer, tls_start - gap
//! ```
//!
//! Variant II: the linker computes `TPOFF = sym_offset - memsz` raw, so the
//! thread pointer sits at `tls_start + memsz`. Variant I: the linker computes
//! `TPOFF = align_up(16, p_align) + sym_offset` from the executable's own
//! `PT_TLS`, so the executable's block is the first one, `gap` above the
//! thread pointer. Either way `tls_start` carries the largest alignment any
//! module asked for. Every input is a sum of numbers a file declared, which is
//! why every step here is checked and the whole thing is a pure function:
//! `dtv_bytes <= tls_start` is the property, and it used to be an assertion in
//! the kernel reached from a crafted `PT_TLS`.

use crate::header::Machine;

/// Which of the psABIs' two TLS layouts a machine uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    /// The thread pointer addresses a two-word TCB, `[DTV, reserved]`, and the
    /// first module's data follows it.
    I,
    /// The thread pointer addresses a TCB, `[self, DTV]`, after the last
    /// module's data.
    II,
}

impl Variant {
    pub const fn of(machine: Machine) -> Variant {
        match machine {
            Machine::X86_64 => Variant::II,
            Machine::Aarch64 => Variant::I,
        }
    }
}

/// A thread's static TLS, as much of it as its layout depends on. Its
/// alignments are powers of two, or it does not exist ([`Static::new`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Static {
    variant: Variant,
    /// Every static module's bytes, placed by [`place_module`].
    total_memsz: usize,
    /// The largest `p_align` any static module declared, "no constraint" as 8.
    max_align: usize,
    /// The `p_align` of the module at offset 0, "no constraint" as 8: variant
    /// I's executable, whose linker fixed its distance from the thread pointer
    /// from it.
    first_align: usize,
}

/// A planned TLS allocation, in offsets from the base of one block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TlsBlock {
    /// Bytes to allocate, a whole number of granules.
    pub alloc_size: usize,
    /// Where the first module's TLS data begins. Aligned to the requested
    /// alignment, and never below `dtv_bytes`.
    pub tls_start: usize,
    /// Where the thread pointer goes.
    pub tp_offset: usize,
}

impl Static {
    /// `None` for an alignment that is not a power of two: a mask that is not
    /// a mask can place the data anywhere. Zero and one mean "no constraint".
    pub fn new(variant: Variant, total_memsz: usize, max_align: usize, first_align: usize) -> Option<Static> {
        Some(Static { variant, total_memsz, max_align: effective(max_align)?, first_align: effective(first_align)? })
    }

    pub fn variant(self) -> Variant {
        self.variant
    }

    pub fn total_memsz(self) -> usize {
        self.total_memsz
    }

    pub fn max_align(self) -> usize {
        self.max_align
    }

    /// No module at all: what a thread of a program without TLS is given.
    pub const fn empty(variant: Variant) -> Static {
        Static { variant, total_memsz: 0, max_align: 8, first_align: 8 }
    }

    /// Lay out one thread's TLS block, or `None` for a layout no allocation
    /// can hold.
    ///
    /// `granule` is the allocation unit the block is rounded up to,
    /// `dtv_bytes` the fixed header the kernel writes at offset zero, and
    /// `tcb_size` variant II's TCB; variant I's is the `gap` below its data.
    /// All three are kernel constants; everything else came out of a file.
    pub fn plan(self, tcb_size: usize, dtv_bytes: usize, granule: usize) -> Option<TlsBlock> {
        debug_assert!(granule.is_power_of_two());
        let align = self.max_align;
        match self.variant {
            Variant::II => {
                let block_size = self.total_memsz.checked_add(tcb_size)?;
                // The DTV goes at the start of this same allocation and the TLS
                // data is placed `align`-aligned above it, so both belong in
                // the size. Sizing from the block and the alignment alone left
                // `tls_start` free to land inside the DTV.
                let alloc_size =
                    align_up(block_size.checked_add(dtv_bytes)?.checked_add(align)?, granule)?;
                // Rounding *down* by `align` loses less than `align`, and
                // `align` was one of the addends — so this is at least
                // `dtv_bytes + 1` and the DTV can never be overwritten by TLS
                // data.
                let tls_start = (alloc_size - block_size) & !(align - 1);
                Some(TlsBlock { alloc_size, tls_start, tp_offset: tls_start + self.total_memsz })
            }
            Variant::I => {
                let gap = self.gap();
                // At least 16, so the thread pointer `gap` below is 16-aligned.
                let tls_start = align_up(dtv_bytes.checked_add(gap)?, align.max(16))?;
                let alloc_size = align_up(tls_start.checked_add(self.total_memsz)?, granule)?;
                Some(TlsBlock { alloc_size, tls_start, tp_offset: tls_start - gap })
            }
        }
    }

    /// A static-TLS datum's initial-exec offset from the thread pointer: psABI
    /// `S + A - tp`, where `module_addr` is `S` (its module's `base_offset`
    /// plus the datum's offset) from `tls_start`. Every `TPOFF` branch passes
    /// the addend here, so none can drop `A`.
    pub fn tpoff(self, module_addr: u64, addend: i64) -> i64 {
        let from_start = module_addr as i64 + addend;
        match self.variant {
            Variant::II => from_start - self.total_memsz as i64,
            Variant::I => from_start + self.gap() as i64,
        }
    }

    /// Variant I's distance from the thread pointer to the first module's
    /// data: `align_up(16, p_align)` of that module, the linker's own.
    fn gap(self) -> usize {
        self.first_align.max(16)
    }
}

/// One module's placement in a combined block: `cursor` rounded up to the
/// module's own `p_align` (psABI, not a shared constant), floored at the 16
/// `cmpxchg16b` needs. `align` is a power of two ≤ [`crate::MAX_TLS_ALIGN`]
/// by [`crate::Layout::parse`]; `tls_start` carries the max, so a base on the
/// module's own align lands the module on it.
pub fn place_module(cursor: usize, memsz: usize, align: usize) -> Option<(usize, usize)> {
    let align = align.max(16);
    let base = if cursor > 0 { align_up(cursor, align)? } else { 0 };
    Some((base, base.checked_add(memsz)?))
}

/// `align`, with "no constraint" as 8; `None` for one that is not a power of two.
fn effective(align: usize) -> Option<usize> {
    if align != 0 && !align.is_power_of_two() {
        return None;
    }
    Some(if align > 1 { align } else { 8 })
}

fn align_up(value: usize, granule: usize) -> Option<usize> {
    value.checked_add(granule - 1).map(|v| v & !(granule - 1))
}
