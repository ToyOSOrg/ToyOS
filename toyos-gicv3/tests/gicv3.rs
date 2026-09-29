use toyos_gicv3::{find_redistributor, packed_affinity, sgi1r, FRAME};

#[test]
fn affinity_drops_mpidr_flags_between_aff2_and_aff3() {
    // Aff3 in 39:32; `U` (30), `MT` (24) and the RES1 bit 31 in between.
    let mpidr = 0x12 << 32 | 1 << 31 | 1 << 30 | 1 << 24 | 0x34_5678;
    assert_eq!(packed_affinity(mpidr), 0x1234_5678);
}

#[test]
fn sgi_names_aff0_by_range_and_bit() {
    // Aff0 0x13 is range 1, bit 3.
    let value = sgi1r(2, 0x0A0B_0C13);
    assert_eq!(value >> 44 & 0xF, 1, "RS");
    assert_eq!(value & 0xFFFF, 1 << 3, "target list");
    assert_eq!(value >> 16 & 0xFF, 0x0C, "Aff1");
    assert_eq!(value >> 24 & 0xF, 2, "INTID");
    assert_eq!(value >> 32 & 0xFF, 0x0B, "Aff2");
    assert_eq!(value >> 48 & 0xFF, 0x0A, "Aff3");
    assert_eq!(value & !(0xFF << 48 | 0xF << 44 | 0xFF << 32 | 0xF << 24 | 0xFF << 16 | 0xFFFF), 0, "IRM and RES0");
}

#[test]
fn sgi_to_cpu_zero_is_bit_zero_of_range_zero() {
    assert_eq!(sgi1r(0, 0), 1);
}

/// A region of redistributors, each `(affinity, vlpis, last)`, as `GICR_TYPER` reads at each one's offset.
fn region(cpus: &[(u32, bool, bool)]) -> (u64, impl Fn(u64) -> u64 + '_) {
    let stride = |vlpis: bool| if vlpis { 4 * FRAME } else { 2 * FRAME };
    let length = cpus.iter().map(|&(_, vlpis, _)| stride(vlpis)).sum();
    let typer = move |at: u64| {
        let mut offset = 0;
        for &(affinity, vlpis, last) in cpus {
            if offset == at {
                return u64::from(affinity) << 32 | u64::from(vlpis) << 1 | u64::from(last) << 4;
            }
            offset += stride(vlpis);
        }
        panic!("a GICR_TYPER read at {at:#x}, which is no redistributor's first frame");
    };
    (length, typer)
}

#[test]
fn walk_steps_over_virtual_lpi_frames() {
    let (length, typer) = region(&[(0, true, false), (1, true, false), (2, true, true)]);
    assert_eq!(find_redistributor(length, 2, typer), Some(8 * FRAME));
}

#[test]
fn walk_steps_two_frames_without_them() {
    let (length, typer) = region(&[(0, false, false), (1, false, true)]);
    assert_eq!(find_redistributor(length, 1, typer), Some(2 * FRAME));
}

#[test]
fn walk_stops_at_the_last() {
    let (_, typer) = region(&[(0, false, false), (1, false, true)]);
    assert_eq!(find_redistributor(16 * FRAME, 7, typer), None);
}

#[test]
fn walk_stops_at_the_region_end() {
    let (length, typer) = region(&[(0, false, false), (1, false, false)]);
    assert_eq!(find_redistributor(length, 7, typer), None);
}
