//! The TLS block's arithmetic, over the whole space a file can name.
//!
//! `total_memsz` and `align` are sums of numbers a file declared, and the
//! kernel used to assert `dtv_size < tls_start` after computing them — a
//! kernel-bug assert reached from a crafted `PT_TLS`. The property is proved
//! here instead, which is what lets the assert go.

use toyos_elf::tls::{self, Static, Variant};

const TCB: usize = 64;
const DTV: usize = 16 + 64 * 8;
const GRANULE: usize = 2 * 1024 * 1024;

#[test]
fn the_dtv_is_never_overwritten_by_tls_data() {
    let sizes = [
        0usize,
        1,
        8,
        4095,
        4096,
        DTV,
        DTV + 1,
        0x1F_FF00,
        GRANULE - TCB - 1,
        GRANULE,
        GRANULE + 1,
        16 * GRANULE,
    ];
    let aligns = [0usize, 1, 2, 8, 16, 64, 4096, 65536, GRANULE];
    for &memsz in &sizes {
        for &align in &aligns {
            let plan = Static::new(Variant::II, memsz, align, align)
                .and_then(|s| s.plan(TCB, DTV, GRANULE))
                .unwrap_or_else(|| panic!("no plan for memsz {memsz} align {align}"));
            let effective = if align > 1 { align } else { 8 };
            assert!(
                plan.tls_start >= DTV,
                "memsz {memsz} align {align}: TLS data at {} overlaps a {DTV}-byte DTV",
                plan.tls_start,
            );
            assert_eq!(plan.tls_start % effective, 0, "memsz {memsz} align {align}");
            assert_eq!(plan.tp_offset, plan.tls_start + memsz);
            assert!(
                plan.tp_offset + TCB <= plan.alloc_size,
                "memsz {memsz} align {align}: the TCB runs past the allocation",
            );
            assert_eq!(plan.alloc_size % GRANULE, 0, "memsz {memsz} align {align}");
        }
    }
}

#[test]
fn a_size_no_allocation_can_hold_has_no_plan() {
    for variant in [Variant::I, Variant::II] {
        let plan = |memsz, align| Static::new(variant, memsz, align, align).unwrap().plan(TCB, DTV, GRANULE);
        assert_eq!(plan(usize::MAX, 8), None, "{variant:?}");
        assert_eq!(plan(usize::MAX - TCB, 8), None, "{variant:?}");
        assert_eq!(plan(usize::MAX - GRANULE, GRANULE), None, "{variant:?}");
    }
}

/// `!(align - 1)` is only a mask for a power of two, and a mask that is not a
/// mask can place the TLS data anywhere — the DTV included.
#[test]
fn an_alignment_that_is_not_a_power_of_two_has_no_plan() {
    for align in [3usize, 5, 6, 100, usize::MAX] {
        for variant in [Variant::I, Variant::II] {
            assert_eq!(Static::new(variant, 64, align, 8), None, "align {align}");
            assert_eq!(Static::new(variant, 64, 8, align), None, "first align {align}");
        }
    }
}

#[test]
fn modules_are_placed_in_order_with_the_first_at_zero() {
    // The first module lands at 0 whatever its align; a later one rounds up to
    // its own align, floored at 16.
    let (base, cursor) = tls::place_module(0, 0x48, 8).unwrap();
    assert_eq!((base, cursor), (0, 0x48));
    let (base, cursor) = tls::place_module(cursor, 0x10, 16).unwrap();
    assert_eq!((base, cursor), (0x50, 0x60));
    let (base, cursor) = tls::place_module(cursor, 0, 8).unwrap();
    assert_eq!((base, cursor), (0x60, 0x60));
    assert_eq!(tls::place_module(0x60, usize::MAX, 16), None);
}

/// psABI variant II oracle (`std_tls`): a 160-byte lib ahead of a 64-aligned exe
/// places it at `align_up(160, 64) == 192`; the old constant 16 gave 160.
#[test]
fn a_module_lands_on_its_own_declared_alignment() {
    assert_eq!(tls::place_module(160, 152, 64), Some((192, 344)));
    assert_eq!(tls::place_module(160, 152, 16), Some((160, 312)));
    for &align in &[0usize, 1, 2, 8, 16, 32, 64, 4096, 65536] {
        let (base, _) = tls::place_module(0xA5, 8, align).unwrap();
        assert_eq!(base % align.max(16), 0, "align {align}: base {base}");
        assert!(base >= 0xA5, "align {align}: base {base} overlaps the cursor");
    }
}

/// `TPOFF64`/`TPOFF32` is `S + A - tp`: the addend is in every branch's answer.
#[test]
fn tpoff_carries_the_addend() {
    let total = 0x200usize;
    let two = Static::new(Variant::II, total, 64, 64).unwrap();
    let one = Static::new(Variant::I, total, 64, 64).unwrap();
    for &module_addr in &[0u64, 8, 0x40, 0x1F0] {
        assert_eq!(two.tpoff(module_addr, 0), module_addr as i64 - total as i64);
        assert_eq!(one.tpoff(module_addr, 0), module_addr as i64 + 64);
        for &addend in &[0i64, 8, -8, 0x100, -0x100] {
            for s in [one, two] {
                assert_eq!(s.tpoff(module_addr, addend) - s.tpoff(module_addr, 0), addend, "{s:?}");
            }
        }
    }
}

/// Variant I, as lld resolves an AArch64 executable's own local-exec access
/// at link time: `TPOFF = align_up(16, p_align) + offset in its PT_TLS` —
/// lld's `getTlsTpOffset`, and the AArch64 ELF ABI's 16-byte TCB. The
/// executable is the module at offset 0, so its data has to start exactly
/// that far above the thread pointer, whatever a later module asks for.
#[test]
fn variant_i_puts_the_first_module_where_its_linker_put_it() {
    let sizes = [0usize, 1, 8, 0x48, 4096, DTV + 1, GRANULE - 1, GRANULE + 1];
    let aligns = [0usize, 1, 2, 8, 16, 64, 4096, GRANULE];
    for &memsz in &sizes {
        for &first in &aligns {
            for &max in aligns.iter().filter(|&&a| a.max(8) >= first.max(8)) {
                let s = Static::new(Variant::I, memsz, max, first).unwrap();
                let plan = s.plan(TCB, DTV, GRANULE).unwrap();
                let gap = 16usize.max(first);
                let at = format!("memsz {memsz} first {first} max {max}: {plan:?}");
                assert_eq!(plan.tls_start - plan.tp_offset, gap, "{at}");
                assert_eq!(s.tpoff(0, 0), gap as i64, "{at}");
                assert!(plan.tp_offset >= DTV, "{at}: the TCB overlaps the DTV");
                assert_eq!(plan.tp_offset % 16, 0, "{at}");
                assert_eq!(plan.tls_start % max.max(16), 0, "{at}");
                assert!(plan.tls_start + memsz <= plan.alloc_size, "{at}");
                assert_eq!(plan.alloc_size % GRANULE, 0, "{at}");
            }
        }
    }
}

/// rust-lld's own answer, read off an `aarch64-unknown-toyos` executable whose
/// `PT_TLS` is 64-aligned: `add x0, x8, #0x40` for the datum at offset 0 of
/// `.tdata`, `#0x80` for the one at 0x40.
#[test]
fn variant_i_agrees_with_what_lld_linked() {
    let s = Static::new(Variant::I, 0xb0, 64, 64).unwrap();
    assert_eq!(s.tpoff(0, 0), 0x40);
    assert_eq!(s.tpoff(0x40, 0), 0x80);
}

/// The machine names the variant: x86-64's psABI is variant II, AArch64's
/// variant I.
#[test]
fn each_machine_has_its_own_variant() {
    assert_eq!(Variant::of(toyos_elf::Machine::X86_64), Variant::II);
    assert_eq!(Variant::of(toyos_elf::Machine::Aarch64), Variant::I);
}
