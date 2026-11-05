//! Where a thread's area puts its DTV, its executable's TLS data and its
//! thread control block, in the layout its machine's psABI names
//! ([`Variant`]), with the DTV at the area's first byte:
//!
//! ```text
//! variant II (x86-64):  [DTV] [pad] [data] [TCB]          TP at the TCB
//! variant I (AArch64):  [DTV] [pad] [TCB] [gap] [data]    TP at the TCB
//! ```
//!
//! Variant II's linker fixed every local-exec offset as `S - round(memsz,
//! p_align)`, so the executable's data, so rounded, ends at TP. Variant I's
//! fixed them as `align_up(16, p_align) + S`, so the data starts that far
//! above TP. Every input is a number the executable declared.

/// Which of the psABIs' two TLS layouts a machine uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    /// TP addresses a two-word TCB, `[DTV, implementation]`, and the
    /// executable's data follows it.
    I,
    /// TP addresses a TCB, `[self, DTV, ...]`, after the executable's data.
    II,
}

/// One thread's area, in offsets from its first byte.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Area {
    /// Bytes from the first to past the last.
    pub size: usize,
    /// What the first byte is aligned to: a power of two, at least 16.
    pub align: usize,
    /// Where the executable's TLS data starts.
    pub data: usize,
    /// Where the thread pointer goes.
    pub tp: usize,
}

/// The area for an executable whose `PT_TLS` is `memsz` bytes declared
/// aligned to `p_align` (`memsz` zero for none), behind `dtv` bytes of DTV and
/// beside a `tcb`-byte TCB. `None` for an alignment that is not a power of two
/// or a size no `usize` holds.
pub fn area(variant: Variant, memsz: usize, p_align: usize, dtv: usize, tcb: usize) -> Option<Area> {
    // Zero and one are "no constraint", as the ELF specification has them.
    let p_align = p_align.max(1);
    if !p_align.is_power_of_two() {
        return None;
    }
    let align = p_align.max(16);
    match variant {
        Variant::II => {
            let extent = align_up(memsz, p_align)?;
            let tp = align_up(dtv.checked_add(extent)?, align)?;
            Some(Area { size: tp.checked_add(tcb)?, align, data: tp - extent, tp })
        }
        Variant::I => {
            let tp = align_up(dtv, align)?;
            let data = tp.checked_add(align)?;
            let size = data.checked_add(memsz)?.max(tp.checked_add(tcb)?);
            Some(Area { size, align, data, tp })
        }
    }
}

/// `value` rounded up to `granule`, a power of two.
pub(crate) fn align_up(value: usize, granule: usize) -> Option<usize> {
    value.checked_add(granule - 1).map(|v| v & !(granule - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DTV: usize = 24 + 16 * 8;

    /// Every size and alignment a file can declare up to a few pages, both
    /// variants: the data is on its alignment, clear of the DTV and the TCB,
    /// inside the area, and exactly where the linker put the thread pointer
    /// against it.
    #[test]
    fn the_data_is_where_the_linker_fixed_it_and_clear_of_everything_else() {
        for (variant, tcb) in [(Variant::II, 64), (Variant::I, 16)] {
            for memsz in (0..300).chain([4095, 4096, 4097, 3 * 4096 + 5]) {
                for p_align in [0, 1, 2, 4, 8, 16, 32, 64, 4096, 1 << 21] {
                    let a = area(variant, memsz, p_align, DTV, tcb).unwrap();
                    let p = p_align.max(1);
                    assert_eq!(a.data % p, 0, "{variant:?} memsz {memsz} align {p_align}: data off its alignment");
                    assert!(a.align.is_power_of_two() && a.align >= 16 && a.align % p == 0);
                    assert!(a.data >= DTV, "the data meets the DTV");
                    assert!(a.data + memsz <= a.size && a.tp + tcb <= a.size, "past the area's end");
                    assert_eq!(a.tp % 16, 0, "a thread pointer off 16");
                    match variant {
                        Variant::II => {
                            assert_eq!(a.data + memsz.next_multiple_of(p), a.tp, "variant II's data ends at TP");
                        }
                        Variant::I => {
                            assert_eq!(a.data, a.tp + p.max(16), "variant I's data starts align_up(16, p_align) past TP");
                            assert!(a.tp >= DTV);
                        }
                    }
                }
            }
        }
    }

    /// rust-lld's own answers. AArch64, an `aarch64-unknown-toyos` executable
    /// whose `PT_TLS` is 0xb0 bytes aligned to 64: `add x0, x8, #0x40` for the
    /// datum at offset 0 of `.tdata`, `#0x80` for the one at 0x40. x86-64: a
    /// local-exec access is `S - round(p_memsz, p_align)`, so 0xa8 bytes aligned
    /// to 0x40 end 0xc0 below TP.
    #[test]
    fn the_data_is_where_lld_linked_it() {
        let a = area(Variant::I, 0xb0, 64, DTV, 16).unwrap();
        assert_eq!((a.data - a.tp, a.data + 0x40 - a.tp), (0x40, 0x80));
        let a = area(Variant::II, 0xa8, 0x40, DTV, 64).unwrap();
        assert_eq!(a.tp - a.data, 0xc0);
    }

    #[test]
    fn an_alignment_that_is_no_power_of_two_or_a_size_past_the_space_is_refused() {
        assert_eq!(area(Variant::II, 8, 24, DTV, 64), None);
        assert_eq!(area(Variant::I, 8, 3, DTV, 16), None);
        assert_eq!(area(Variant::II, usize::MAX - 8, 16, DTV, 64), None);
        assert_eq!(area(Variant::I, usize::MAX - 8, 16, DTV, 16), None);
    }
}
