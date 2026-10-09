//! The IORT: QEMU `virt`'s own tables in both of its GIC modes, decoded
//! against what was read off their bytes by hand with DEN 0049 open, and
//! every refusal over crafted ones.

mod common;

use core::cell::Cell;
use core::num::NonZeroU32;

use common::{declare_len, reseal, rsdp, sdt, xsdt, Machine};
use toyos_acpi::{
    find_table, iort, madt_entries, IortRefused, ItsDevice, MadtEntry, Node, Phys, Route, Smmuv3, TableError,
    MADT_ENTRIES,
};

const RSDP: u64 = 0x4cb4_3018;
const XSDT: u64 = 0x4cb4_3f18;
const APIC: u64 = 0x4cb4_3c98;
const IORT: u64 = 0x4cb4_3198;

const VIRT_RSDP: &[u8] = include_bytes!("../fixtures/qemu-11.1.1-virt/rsdp.bin");
const VIRT_XSDT: &[u8] = include_bytes!("../fixtures/qemu-11.1.1-virt/xsdt.bin");
/// `-M virt,gic-version=3,iommu=smmuv3` under HVF: the hypervisor's GIC.
const HWGIC: &[(u64, &[u8])] = &[
    (RSDP, VIRT_RSDP),
    (XSDT, VIRT_XSDT),
    (APIC, include_bytes!("../fixtures/qemu-11.1.1-virt/hwgic/apic.bin")),
    (IORT, include_bytes!("../fixtures/qemu-11.1.1-virt/hwgic/iort.bin")),
];
/// `-M virt,gic-version=3,its=on,iommu=smmuv3,kernel-irqchip=off`.
const ITS: &[(u64, &[u8])] = &[
    (RSDP, VIRT_RSDP),
    (XSDT, VIRT_XSDT),
    (APIC, include_bytes!("../fixtures/qemu-11.1.1-virt/its/apic.bin")),
    (IORT, include_bytes!("../fixtures/qemu-11.1.1-virt/its/iort.bin")),
];

/// The node both modes publish: `arm-smmuv3` at `0x09050000`, `COHACC
/// override` set, and the GSIVs in the node's order — event 106, PRI 107,
/// GERR 109, sync 108 — of which GERR is the higher of the last two. Every
/// one is wired, so the revision 4 node's DeviceID mapping index is ignored
/// and its one mapping in the ITS boot routes streams.
fn virt_smmu() -> Smmuv3 {
    Smmuv3 {
        base: 0x0905_0000,
        coherent_override: true,
        event: NonZeroU32::new(106),
        gerror: NonZeroU32::new(109),
        sync: NonZeroU32::new(108),
    }
}

#[test]
fn the_hardware_gic_boot_routes_bus_zero_through_the_smmu_and_nothing_to_an_its() {
    let table = iort(Machine { regions: HWGIC }, RSDP).expect("the IORT QEMU published");
    assert_eq!(table.nodes().collect::<Vec<_>>(), [Node::Smmuv3(virt_smmu()), Node::RootComplex { segment: 0 }]);
    for rid in [0x00, 0x08, 0xff] {
        assert_eq!(
            table.route(0, rid),
            Ok(Route::Translated { smmu: virt_smmu(), stream: u32::from(rid), its: None }),
            "requester {rid:#x}"
        );
    }
    // The one mapping is `0x00..=0xff`: bus 1 is mapped nowhere, and neither is another segment.
    assert_eq!(table.route(0, 0x100), Ok(Route::Unmapped));
    assert_eq!(table.route(0, 0xffff), Ok(Route::Unmapped));
    assert_eq!(table.route(1, 0x08), Ok(Route::Unmapped));
}

#[test]
fn the_its_boot_routes_bus_zero_through_the_smmu_to_the_its_and_the_rest_past_it() {
    let table = iort(Machine { regions: ITS }, RSDP).expect("the IORT QEMU published");
    assert_eq!(
        table.nodes().collect::<Vec<_>>(),
        [Node::Its { id: 0 }, Node::Smmuv3(virt_smmu()), Node::RootComplex { segment: 0 }]
    );
    for rid in [0x00, 0x08, 0xff] {
        let id = u32::from(rid);
        assert_eq!(
            table.route(0, rid),
            Ok(Route::Translated { smmu: virt_smmu(), stream: id, its: Some(ItsDevice { its: 0, device: id }) }),
            "requester {rid:#x}"
        );
    }
    for rid in [0x100, 0x1234, 0xffff] {
        assert_eq!(
            table.route(0, rid),
            Ok(Route::Untranslated(ItsDevice { its: 0, device: u32::from(rid) })),
            "requester {rid:#x}"
        );
    }
    assert_eq!(table.route(1, 0x08), Ok(Route::Unmapped));
}

fn madt_of(regions: &[(u64, &[u8])]) -> Vec<MadtEntry> {
    let table = find_table(Machine { regions }, RSDP, b"APIC", MADT_ENTRIES).expect("MADT");
    madt_entries(&table).map(|entry| entry.expect("a structure the list holds whole")).collect()
}

/// `arm-gicv3-its` at `0x08080000`, under the id the IORT's ITS node carries.
#[test]
fn the_its_boots_madt_names_the_its_the_iort_routes_to() {
    let its: Vec<_> = madt_of(ITS).into_iter().filter(|entry| matches!(entry, MadtEntry::Its { .. })).collect();
    assert_eq!(its, [MadtEntry::Its { id: 0, base: 0x0808_0000 }]);
}

/// It ends in a GIC MSI Frame structure, type 0xD, which is no ITS.
#[test]
fn the_hardware_gic_boots_madt_names_no_its() {
    let entries = madt_of(HWGIC);
    assert!(!entries.iter().any(|entry| matches!(entry, MadtEntry::Its { .. })));
    assert_eq!(entries.last(), Some(&MadtEntry::Other(0xD)));
}

// --- crafted tables -------------------------------------------------------

const TABLE_AT: u64 = 0x9000;

/// One node: Table 4's header over `fields`, then `mappings`, the ID array
/// placed straight after the fields.
fn node(kind: u8, revision: u8, fields: &[u8], mappings: &[[u32; 5]]) -> Vec<u8> {
    let mut n = vec![kind, 0, 0, revision, 0, 0, 0, 0];
    n.extend((mappings.len() as u32).to_le_bytes());
    n.extend(if mappings.is_empty() { 0u32 } else { 16 + fields.len() as u32 }.to_le_bytes());
    n.extend(fields);
    for word in mappings.iter().flatten() {
        n.extend(word.to_le_bytes());
    }
    let len = n.len() as u16;
    n[1..3].copy_from_slice(&len.to_le_bytes());
    n
}

fn its(id: u32) -> Vec<u8> {
    node(0, 1, &[1u32.to_le_bytes(), id.to_le_bytes()].concat(), &[])
}

/// A revision 3 root complex of `segment`: 20 bytes of fields.
fn root_complex(segment: u32, mappings: &[[u32; 5]]) -> Vec<u8> {
    let mut fields = vec![0u8; 20];
    fields[12..16].copy_from_slice(&segment.to_le_bytes());
    node(2, 3, &fields, mappings)
}

/// An SMMUv3 node of `revision` at `0x0905_0000`, 52 bytes of fields: Table
/// 13's flags (24), its event, PRI, GERR and sync GSIVs (44..60) and its
/// DeviceID mapping index (64).
fn smmu_with(revision: u8, flags: u32, gsivs: [u32; 4], index: u32, mappings: &[[u32; 5]]) -> Vec<u8> {
    let mut fields = vec![0u8; 52];
    fields[..8].copy_from_slice(&0x0905_0000u64.to_le_bytes());
    fields[8..12].copy_from_slice(&flags.to_le_bytes());
    for (i, gsiv) in gsivs.iter().enumerate() {
        fields[28 + 4 * i..32 + 4 * i].copy_from_slice(&gsiv.to_le_bytes());
    }
    fields[48..52].copy_from_slice(&index.to_le_bytes());
    node(4, revision, &fields, mappings)
}

/// A revision 5 SMMUv3 with no flag and no GSIV: none of its mappings is its own.
fn smmu(mappings: &[[u32; 5]]) -> Vec<u8> {
    smmu_with(5, 0, [0; 4], 0, mappings)
}

/// An IORT of `revision` over `nodes`, its array straight after the header.
fn table(revision: u8, nodes: &[Vec<u8>]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend((nodes.len() as u32).to_le_bytes());
    body.extend(48u32.to_le_bytes());
    body.extend(0u32.to_le_bytes());
    body.extend(nodes.concat());
    sdt(b"IORT", revision, &body)
}

/// Where each of three nodes laid out as `[its, smmu, root complex]` starts:
/// the root complex after an SMMUv3 of no mapping, and of one.
const ITS_AT: u32 = 48;
const SMMU_AT: u32 = ITS_AT + 24;
const ROOT_AT: u32 = SMMU_AT + 68;
const ROOT_AFTER_ONE: u32 = ROOT_AT + 20;

/// The ITS boot's shape with every number chosen: requesters `0x100..=0x1ff`
/// to streams `0x1000..`, streams `0x1000..=0x10ff` to DeviceIDs `0x20000..`.
fn three(root: &[[u32; 5]], onward: &[[u32; 5]]) -> Vec<u8> {
    table(5, &[its(7), smmu(onward), root_complex(0, root)])
}

fn judged<T>(bytes: &[u8], ask: impl FnOnce(&toyos_acpi::Iort<Machine<'_>>) -> T) -> Result<T, IortRefused> {
    let head = rsdp(0x1000, 2, 36);
    let root = xsdt(&[TABLE_AT]);
    let regions: &[(u64, &[u8])] = &[(0x800, &head), (0x1000, &root), (TABLE_AT, bytes)];
    iort(Machine { regions }, 0x800).map(|table| ask(&table))
}

fn refusal(bytes: &[u8]) -> Option<IortRefused> {
    judged(bytes, |_| ()).err()
}

fn route(bytes: &[u8], segment: u32, rid: u16) -> Result<Route, IortRefused> {
    judged(bytes, |table| table.route(segment, rid))?
}

fn plain_smmu() -> Smmuv3 {
    Smmuv3 { base: 0x0905_0000, coherent_override: false, event: None, gerror: None, sync: None }
}

#[test]
fn a_mapping_carries_an_id_by_its_offset_from_the_input_base_and_ends_at_its_last_id() {
    let t = three(&[[0x100, 0xff, 0x1000, SMMU_AT, 0]], &[[0x1000, 0xff, 0x2_0000, ITS_AT, 0]]);
    let through = |stream, device| {
        Ok(Route::Translated { smmu: plain_smmu(), stream, its: Some(ItsDevice { its: 7, device }) })
    };
    assert_eq!(route(&t, 0, 0x100), through(0x1000, 0x2_0000));
    assert_eq!(route(&t, 0, 0x180), through(0x1080, 0x2_0080));
    // `Number of IDs` is the count minus one: the last ID is in, the next is out.
    assert_eq!(route(&t, 0, 0x1ff), through(0x10ff, 0x2_00ff));
    assert_eq!(route(&t, 0, 0x200), Ok(Route::Unmapped));
    assert_eq!(route(&t, 0, 0x0ff), Ok(Route::Unmapped));
}

#[test]
fn a_stream_the_smmu_maps_nowhere_has_no_its() {
    let t = three(&[[0, 0x1ff, 0, SMMU_AT, 0]], &[[0, 0xff, 0, ITS_AT, 0]]);
    assert_eq!(route(&t, 0, 0x100), Ok(Route::Translated { smmu: plain_smmu(), stream: 0x100, its: None }));
}

#[test]
fn a_revision_whose_layout_was_not_read_is_refused() {
    for revision in [0, 4, 8, 0xff] {
        assert_eq!(refusal(&table(revision, &[its(0)])), Some(IortRefused::Revision(revision)));
    }
    for revision in [5, 6, 7] {
        assert_eq!(refusal(&table(revision, &[its(0)])), None);
    }
}

#[test]
fn a_table_too_short_for_its_own_header_fields_is_refused() {
    let mut t = table(5, &[]);
    declare_len(&mut t, 44);
    assert_eq!(refusal(&t), Some(IortRefused::Table(TableError::Length { declared: 44, needed: 48 })));
}

#[test]
fn a_node_array_inside_the_header_is_refused() {
    let mut t = table(5, &[its(0)]);
    t[40..44].copy_from_slice(&40u32.to_le_bytes());
    reseal(&mut t);
    assert_eq!(refusal(&t), Some(IortRefused::NodeArray { offset: 40 }));
}

#[test]
fn a_node_count_that_overruns_the_table_is_refused_where_the_bytes_end() {
    let mut t = table(5, &[its(0), its(1)]);
    t[36..40].copy_from_slice(&3u32.to_le_bytes());
    reseal(&mut t);
    assert_eq!(refusal(&t), Some(IortRefused::Node { at: 96, declared: 0 }));
    // A count nothing could hold costs one refusal, not a walk of its length.
    t[36..40].copy_from_slice(&u32::MAX.to_le_bytes());
    reseal(&mut t);
    assert_eq!(refusal(&t), Some(IortRefused::Node { at: 96, declared: 0 }));
}

#[test]
fn a_node_array_past_the_table_is_refused_as_its_first_node() {
    let mut t = table(5, &[its(0)]);
    t[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
    reseal(&mut t);
    assert_eq!(refusal(&t), Some(IortRefused::Node { at: u32::MAX as usize, declared: 0 }));
}

#[test]
fn a_node_longer_than_the_table_or_shorter_than_its_type_is_refused() {
    let mut long = table(5, &[its(0)]);
    long[49..51].copy_from_slice(&25u16.to_le_bytes());
    reseal(&mut long);
    assert_eq!(refusal(&long), Some(IortRefused::Node { at: 48, declared: 25 }));

    // Zero would never advance the walk; eight is below Table 4's own header.
    for declared in [0u16, 8, 23] {
        let mut short = table(5, &[its(0)]);
        short[49..51].copy_from_slice(&declared.to_le_bytes());
        reseal(&mut short);
        assert_eq!(refusal(&short), Some(IortRefused::Node { at: 48, declared: usize::from(declared) }));
    }

    // An SMMUv3 node cut before its DeviceID mapping index, with bytes after it to read.
    let mut cut = table(5, &[smmu(&[]), its(0)]);
    cut[49..51].copy_from_slice(&64u16.to_le_bytes());
    reseal(&mut cut);
    assert_eq!(refusal(&cut), Some(IortRefused::Node { at: 48, declared: 64 }));
}

#[test]
fn a_node_revision_whose_layout_was_not_read_is_refused() {
    let revised = |mut node: Vec<u8>, revision: u8| {
        node[3] = revision;
        refusal(&table(5, &[node]))
    };
    for (kind, node, known, unknown) in [
        (0u8, its(0), &[1u8][..], &[0u8, 2][..]),
        (2, root_complex(0, &[]), &[3, 4], &[2, 5]),
        (4, smmu_with(5, 0, WIRED, 0, &[]), &[4, 5], &[3, 6]),
    ] {
        for &revision in known {
            assert_eq!(revised(node.clone(), revision), None, "type {kind} revision {revision}");
        }
        for &revision in unknown {
            assert_eq!(
                revised(node.clone(), revision),
                Some(IortRefused::NodeRevision { at: 48, kind, revision }),
                "type {kind} revision {revision}"
            );
        }
    }
}

/// A root complex whose ID array is `offset` into it and which is `pad` bytes
/// longer than its revision 3 fields.
fn root_with_array_at(revision: u8, offset: u32, pad: usize) -> Vec<u8> {
    let mut node = node(2, revision, &vec![0u8; 20 + pad], &[[0, 0, 0, 48, 0]]);
    node[12..16].copy_from_slice(&offset.to_le_bytes());
    node
}

#[test]
fn an_id_array_lying_over_its_nodes_own_fields_is_refused() {
    let with = |root: Vec<u8>| refusal(&table(5, &[its(0), root]));
    // Revision 3's fields end at 36 and revision 4's, with its PASID capabilities and flags, at 40.
    assert_eq!(with(root_with_array_at(3, 36, 0)), None);
    assert_eq!(with(root_with_array_at(3, 32, 0)), Some(IortRefused::Mappings { at: 72, count: 1, offset: 32 }));
    assert_eq!(with(root_with_array_at(4, 40, 4)), None);
    assert_eq!(with(root_with_array_at(4, 36, 4)), Some(IortRefused::Mappings { at: 72, count: 1, offset: 36 }));
    // And inside the node's header.
    assert_eq!(with(root_with_array_at(3, 0, 0)), Some(IortRefused::Mappings { at: 72, count: 1, offset: 0 }));
}

#[test]
fn an_id_array_running_past_its_node_is_refused() {
    let counted = |count: u32| {
        let mut root = root_complex(0, &[[0, 0, 0, 48, 0], [1, 0, 1, 48, 0]]);
        root[8..12].copy_from_slice(&count.to_le_bytes());
        refusal(&table(5, &[its(0), root]))
    };
    assert_eq!(counted(2), None);
    assert_eq!(counted(3), Some(IortRefused::Mappings { at: 72, count: 3, offset: 36 }));
    // A count whose bytes no 32-bit length could hold.
    assert_eq!(counted(u32::MAX), Some(IortRefused::Mappings { at: 72, count: u32::MAX, offset: 36 }));

    // Past its node and inside the table: the third mapping would be the next node's bytes.
    let mut root = root_complex(0, &[[0, 0, 0, 48, 0], [1, 0, 1, 48, 0]]);
    root[8..12].copy_from_slice(&3u32.to_le_bytes());
    assert_eq!(
        refusal(&table(5, &[its(0), root, its(1)])),
        Some(IortRefused::Mappings { at: 72, count: 3, offset: 36 })
    );

    // The array's start alone past the node.
    let mut root = root_complex(0, &[[0, 0, 0, 48, 0]]);
    root[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(
        refusal(&table(5, &[its(0), root])),
        Some(IortRefused::Mappings { at: 72, count: 1, offset: u32::MAX })
    );
}

#[test]
fn an_its_node_declaring_an_id_array_is_refused() {
    let fields = [1u32.to_le_bytes(), 0u32.to_le_bytes()].concat();
    let node = node(0, 1, &fields, &[[0, 0, 0, 48, 0]]);
    assert_eq!(refusal(&table(5, &[node])), Some(IortRefused::Mappings { at: 48, count: 1, offset: 24 }));
}

#[test]
fn a_mapping_whose_range_leaves_the_id_space_is_refused() {
    let input = three(&[[0xffff_ff00, 0x100, 0, SMMU_AT, 0]], &[]);
    assert_eq!(refusal(&input), Some(IortRefused::Range { at: 140, base: 0xffff_ff00, last: 0x100 }));
    let output = three(&[[0, 0x100, 0xffff_ff00, SMMU_AT, 0]], &[]);
    assert_eq!(refusal(&output), Some(IortRefused::Range { at: 140, base: 0xffff_ff00, last: 0x100 }));
    // The whole space, to its last ID, is a range.
    assert_eq!(refusal(&three(&[[0, u32::MAX, 0, SMMU_AT, 0]], &[])), None);
    let onward = three(&[], &[[1, u32::MAX, 0, ITS_AT, 0]]);
    assert_eq!(refusal(&onward), Some(IortRefused::Range { at: 72, base: 1, last: u32::MAX }));
}

#[test]
fn a_route_through_an_output_reference_that_names_no_node_is_refused() {
    // Zero, the table's own header, the middle of a node, one past the last node.
    for reference in [0, 36, SMMU_AT + 4, ROOT_AT + 56, u32::MAX] {
        let t = three(&[[0, 0xff, 0, reference, 0]], &[]);
        assert_eq!(route(&t, 0, 8), Err(IortRefused::Output { at: 140, reference }), "reference {reference}");
        // A requester the mapping does not hold never follows it.
        assert_eq!(route(&t, 0, 0x100), Ok(Route::Unmapped), "reference {reference}");
    }
    // The SMMUv3's onward mapping is followed by the stream it holds.
    let t = three(&[[0, 0x1ff, 0, SMMU_AT, 0]], &[[0, 0xff, 0, ITS_AT + 4, 0]]);
    assert_eq!(route(&t, 0, 8), Err(IortRefused::Output { at: 72, reference: ITS_AT + 4 }));
    assert_eq!(route(&t, 0, 0x100), Ok(Route::Translated { smmu: plain_smmu(), stream: 0x100, its: None }));
}

#[test]
fn a_route_through_an_output_reference_to_a_node_its_source_may_not_reach_is_refused() {
    // A root complex to a root complex — itself.
    assert_eq!(
        route(&three(&[[0, 0xff, 0, ROOT_AT, 0]], &[]), 0, 8),
        Err(IortRefused::Output { at: 140, reference: ROOT_AT })
    );
    // An SMMUv3 to an SMMUv3 — itself — and to a root complex.
    for reference in [SMMU_AT, ROOT_AFTER_ONE] {
        assert_eq!(
            route(&three(&[[0, 0xff, 0, SMMU_AT, 0]], &[[0, 0xff, 0, reference, 0]]), 0, 8),
            Err(IortRefused::Output { at: 72, reference })
        );
    }
    // A root complex to a node type not decoded: an SMMUv1 or v2.
    let old = table(5, &[node(3, 3, &[0u8; 44], &[]), root_complex(0, &[[0, 0xff, 0, 48, 0]])]);
    assert_eq!(route(&old, 0, 8), Err(IortRefused::Output { at: 108, reference: 48 }));
}

#[test]
fn a_node_type_not_decoded_is_answered_by_its_type_and_refuses_nothing() {
    // A named component, a PMCG, a reserved memory range, an IWB, and a reserved type.
    let others = [1u8, 5, 6, 7, 0x80].map(|kind| node(kind, 9, &[0xa5; 12], &[]));
    let mut nodes = vec![its(3)];
    nodes.extend(others);
    nodes.push(root_complex(0, &[[0, 0xff, 0, 48, 0]]));
    let t = table(5, &nodes);
    assert_eq!(
        judged(&t, |table| table.nodes().collect::<Vec<_>>()),
        Ok(vec![
            Node::Its { id: 3 },
            Node::Other(1),
            Node::Other(5),
            Node::Other(6),
            Node::Other(7),
            Node::Other(0x80),
            Node::RootComplex { segment: 0 },
        ])
    );
    assert_eq!(route(&t, 0, 8), Ok(Route::Untranslated(ItsDevice { its: 3, device: 8 })));
}

#[test]
fn a_single_mapping_that_is_not_its_smmus_own_is_refused() {
    // Table 6 has it put every input ID out as one: a root complex's, and an SMMUv3's that its index does not name.
    assert_eq!(refusal(&three(&[[0, 0xff, 0, SMMU_AT, 1]], &[])), Some(IortRefused::SingleMapping { at: 140 }));
    assert_eq!(refusal(&three(&[], &[[0, 0, 0x30, ITS_AT, 1]])), Some(IortRefused::SingleMapping { at: 72 }));
    let beside = smmu_with(5, INDEX_VALID, WIRED, 1, &[[0, 0, 0x30, ITS_AT, 1], [0, 0, 0x31, ITS_AT, 1]]);
    assert_eq!(refusal(&behind(beside)), Some(IortRefused::SingleMapping { at: 72 }));
    // The one it names carries the flag as Table 13 asks, and is no refusal.
    let own = smmu_with(5, INDEX_VALID, WIRED, 0, &[[0, 0, 0x30, ITS_AT, 1]]);
    assert_eq!(refusal(&behind(own.clone())), None);
    assert_eq!(onward(&behind(own)), None);
    // An index past the array names no entry, and is no reading that routes.
    let past = behind(smmu_with(5, INDEX_VALID, WIRED, 1, &[[0, 0xff, 0x100, ITS_AT, 0]]));
    assert_eq!(refusal(&past), Some(IortRefused::OwnMapping { at: 72, index: 1, count: 1 }));
}

#[test]
fn an_smmus_own_mapping_index_at_or_past_its_id_array_is_refused() {
    let one = [[0, 0xff, 0x100, ITS_AT, 0]];
    for index in [1, 2, u32::MAX] {
        let past = behind(smmu_with(5, INDEX_VALID, WIRED, index, &one));
        assert_eq!(refusal(&past), Some(IortRefused::OwnMapping { at: 72, index, count: 1 }), "index {index}");
    }
    assert_eq!(
        refusal(&behind(smmu_with(5, INDEX_VALID, WIRED, 0, &[]))),
        Some(IortRefused::OwnMapping { at: 72, index: 0, count: 0 })
    );
    // Revision 4 reads the index wherever a control interrupt has no GSIV.
    let unwired = [106, 0, 109, 108];
    assert_eq!(
        refusal(&behind(smmu_with(4, 0, unwired, 1, &one))),
        Some(IortRefused::OwnMapping { at: 72, index: 1, count: 1 })
    );
    // Where the node says the field is ignored, no value of it is refused.
    for (revision, flags, gsivs) in [(5, 0, WIRED), (5, 0, [0; 4]), (4, INDEX_VALID, WIRED)] {
        let ignored = behind(smmu_with(revision, flags, gsivs, 1, &one));
        assert_eq!(onward(&ignored), Some(ItsDevice { its: 7, device: 0x108 }), "revision {revision}");
    }
}

/// Requesters `0x00..=0x0f` to streams `0x10..=0x1f` of the SMMUv3 at `SMMU_AT`.
const FIRST: [u32; 5] = [0, 0xf, 0x10, SMMU_AT, 0];

fn alone(stream: u32) -> Result<Route, IortRefused> {
    Ok(Route::Translated { smmu: plain_smmu(), stream, its: None })
}

fn shared(stream: u32, at: u32) -> Result<Route, IortRefused> {
    Err(IortRefused::SharedStream { stream, at: at as usize })
}

#[test]
fn a_stream_another_mapping_puts_out_to_the_same_smmu_is_refused() {
    // Two RID ranges of one root complex onto `0x18..=0x1f`, and a third onto the streams after.
    let t = three(&[FIRST, [0x100, 0xf, 0x18, SMMU_AT, 0], [0x200, 0xf, 0x28, SMMU_AT, 0]], &[]);
    assert_eq!(route(&t, 0, 0x7), alone(0x17));
    assert_eq!(route(&t, 0, 0x8), shared(0x18, ROOT_AT));
    assert_eq!(route(&t, 0, 0xf), shared(0x1f, ROOT_AT));
    assert_eq!(route(&t, 0, 0x100), shared(0x18, ROOT_AT));
    assert_eq!(route(&t, 0, 0x107), shared(0x1f, ROOT_AT));
    assert_eq!(route(&t, 0, 0x108), alone(0x20));
    assert_eq!(route(&t, 0, 0x200), alone(0x28));

    // Two segments onto the same streams of one unit.
    const SECOND: u32 = ROOT_AT + 56;
    let segments = table(5, &[its(7), smmu(&[]), root_complex(0, &[FIRST]), root_complex(1, &[FIRST])]);
    assert_eq!(route(&segments, 0, 8), shared(0x18, SECOND));
    assert_eq!(route(&segments, 1, 8), shared(0x18, ROOT_AT));

    // The same stream numbers of another unit are another unit's streams.
    let units = table(
        5,
        &[its(7), smmu(&[]), root_complex(0, &[FIRST]), smmu(&[]), root_complex(1, &[[0, 0xf, 0x10, SECOND, 0]])],
    );
    assert_eq!(route(&units, 0, 8), alone(0x18));
    assert_eq!(route(&units, 1, 8), alone(0x18));

    // A node of a type not decoded is a requester too: a named component's single mapping
    // puts out its base alone, whatever its span.
    let named = |reference| node(1, 4, &[0; 12], &[[0, 0xf, 0x18, reference, 1]]);
    let t = table(5, &[its(7), smmu(&[]), root_complex(0, &[FIRST]), named(SMMU_AT)]);
    assert_eq!(route(&t, 0, 8), shared(0x18, SECOND));
    assert_eq!(route(&t, 0, 7), alone(0x17));
    assert_eq!(route(&t, 0, 9), alone(0x19));
    // To an ITS, `0x18` is a DeviceID and no stream.
    assert_eq!(route(&table(5, &[its(7), smmu(&[]), root_complex(0, &[FIRST]), named(ITS_AT)]), 0, 8), alone(0x18));
}

/// [`Iort::check`] places only the ID arrays of the types it decodes; a stream
/// is judged alone only once every other node's array is read where its
/// header puts it.
#[test]
fn a_stream_is_not_judged_alone_past_an_id_array_out_of_its_node() {
    const NAMED: usize = (ROOT_AT + 56) as usize;
    let at = |offset: u32, count: u32| {
        let mut named = node(1, 4, &[0; 12], &[[0, 0, 0x30, ITS_AT, 1]]);
        named[8..12].copy_from_slice(&count.to_le_bytes());
        named[12..16].copy_from_slice(&offset.to_le_bytes());
        route(&table(5, &[its(7), smmu(&[]), root_complex(0, &[FIRST]), named, its(9)]), 0, 8)
    };
    assert_eq!(at(28, 1), alone(0x18));
    assert_eq!(at(28, 2), Err(IortRefused::Mappings { at: NAMED, count: 2, offset: 28 }));
    assert_eq!(at(0, 1), Err(IortRefused::Mappings { at: NAMED, count: 1, offset: 0 }));
    assert_eq!(at(12, 1), Err(IortRefused::Mappings { at: NAMED, count: 1, offset: 12 }));
}

/// `virt`'s four GSIVs: every control interrupt wired.
const WIRED: [u32; 4] = [106, 107, 109, 108];
/// Table 14, bit 4: `DeviceID mapping index valid`.
const INDEX_VALID: u32 = 1 << 4;

/// An ITS of id 7, `smmu`, and a root complex sending requesters
/// `0x00..=0xff` to it as the same streams.
fn behind(smmu: Vec<u8>) -> Vec<u8> {
    table(5, &[its(7), smmu, root_complex(0, &[[0, 0xff, 0, SMMU_AT, 0]])])
}

/// Where the SMMUv3's own node sends stream 8.
fn onward(table: &[u8]) -> Option<ItsDevice> {
    match route(table, 0, 8) {
        Ok(Route::Translated { stream: 8, its, .. }) => its,
        other => panic!("requester 8 is stream 8 of the SMMUv3: {other:?}"),
    }
}

#[test]
fn the_mapping_a_revision_5_smmu_indexes_is_the_units_own_and_routes_no_stream() {
    // An input base and length no range holds: they are not the table's to judge.
    let wide = behind(smmu_with(5, INDEX_VALID, WIRED, 0, &[[0xffff_ffff, 0xffff_ffff, 0x30, ITS_AT, 0]]));
    assert_eq!(refusal(&wide), None);
    assert_eq!(onward(&wide), None);
    // And ones that would hold the stream: the entry is the unit's DeviceID and no stream's.
    let plausible = behind(smmu_with(5, INDEX_VALID, WIRED, 0, &[[0, 0xffff, 0x30, ITS_AT, 0]]));
    assert_eq!(onward(&plausible), None);
    // The index picks the entry, and the other one still routes.
    let second = smmu_with(5, INDEX_VALID, WIRED, 1, &[[0, 0xff, 0x100, ITS_AT, 0], [0, 0xffff, 0x30, ITS_AT, 0]]);
    assert_eq!(onward(&behind(second)), Some(ItsDevice { its: 7, device: 0x108 }));
    // Flag clear, the index is ignored whatever the GSIVs say.
    let ignored = behind(smmu_with(5, 0, [0; 4], 0, &[[0, 0xff, 0x100, ITS_AT, 0]]));
    assert_eq!(onward(&ignored), Some(ItsDevice { its: 7, device: 0x108 }));
}

#[test]
fn a_revision_4_smmu_indexes_its_own_mapping_whenever_a_control_interrupt_is_not_wired() {
    for unwired in 0..4 {
        let mut gsivs = WIRED;
        gsivs[unwired] = 0;
        let own = behind(smmu_with(4, 0, gsivs, 0, &[[0xffff_ffff, 0xffff_ffff, 0x30, ITS_AT, 0]]));
        assert_eq!(refusal(&own), None, "GSIV {unwired} zero");
        assert_eq!(onward(&own), None, "GSIV {unwired} zero");
        let plausible = behind(smmu_with(4, 0, gsivs, 0, &[[0, 0xffff, 0x30, ITS_AT, 0]]));
        assert_eq!(onward(&plausible), None, "GSIV {unwired} zero");
    }
    // Every one wired, as on `virt`: the field is ignored, and bit 4 of the flags is reserved here.
    for flags in [0, INDEX_VALID] {
        let wired = behind(smmu_with(4, flags, WIRED, 0, &[[0, 0xff, 0x100, ITS_AT, 0]]));
        assert_eq!(onward(&wired), Some(ItsDevice { its: 7, device: 0x108 }));
    }
}

/// Only an SMMUv3 has a DeviceID mapping index. Read as one, this root
/// complex would have no GSIV and, where the index is, its second mapping's
/// output base: zero, naming the first.
#[test]
fn a_root_complex_has_no_mapping_of_its_own() {
    let t = table(5, &[its(7), root_complex(0, &[[0, 0xff, 0, ITS_AT, 0], [0x100, 0xff, 0, ITS_AT, 0]])]);
    assert_eq!(route(&t, 0, 8), Ok(Route::Untranslated(ItsDevice { its: 7, device: 8 })));
}

#[test]
fn an_its_group_of_other_than_one_its_is_refused() {
    for count in [0u32, 2] {
        let group = node(0, 1, &[count.to_le_bytes(), 0u32.to_le_bytes(), 1u32.to_le_bytes()].concat(), &[]);
        assert_eq!(refusal(&table(5, &[group])), Some(IortRefused::ItsGroup { at: 48, count }));
    }
}

#[test]
fn an_id_two_mappings_claim_is_refused_when_it_is_asked_for() {
    // In one root complex: `0x00..=0xff` and `0x80..=0x17f`.
    let t = three(&[[0, 0xff, 0, SMMU_AT, 0], [0x80, 0xff, 0x80, ITS_AT, 0]], &[]);
    assert_eq!(route(&t, 0, 0x7f), Ok(Route::Translated { smmu: plain_smmu(), stream: 0x7f, its: None }));
    assert_eq!(route(&t, 0, 0x80), Err(IortRefused::Overlap { id: 0x80 }));
    assert_eq!(route(&t, 0, 0x100), Ok(Route::Untranslated(ItsDevice { its: 7, device: 0x100 })));

    // Across two root complexes of one segment; a second segment's is its own.
    let two = |segment| {
        table(5, &[its(7), root_complex(0, &[[0, 0xff, 0, 48, 0]]), root_complex(segment, &[[0, 0xff, 0x100, 48, 0]])])
    };
    assert_eq!(route(&two(0), 0, 8), Err(IortRefused::Overlap { id: 8 }));
    assert_eq!(route(&two(1), 0, 8), Ok(Route::Untranslated(ItsDevice { its: 7, device: 8 })));
    assert_eq!(route(&two(1), 1, 8), Ok(Route::Untranslated(ItsDevice { its: 7, device: 0x108 })));

    // In the SMMUv3, by the stream.
    let onward = three(&[[0, 0xff, 0x40, SMMU_AT, 0]], &[[0, 0xff, 0, ITS_AT, 0], [0x48, 0, 0x900, ITS_AT, 0]]);
    assert_eq!(route(&onward, 0, 8), Err(IortRefused::Overlap { id: 0x48 }));
    assert_eq!(
        route(&onward, 0, 9),
        Ok(Route::Translated { smmu: plain_smmu(), stream: 0x49, its: Some(ItsDevice { its: 7, device: 0x49 }) })
    );
}

/// Firmware's bytes, any of them wrong: every single-byte change to either
/// captured IORT, and every length it could declare, is decoded or refused and
/// every requester of two buses asked for, without a panic and to an end.
#[test]
fn no_corruption_of_a_published_iort_panics_or_hangs_the_decoder() {
    for regions in [HWGIC, ITS] {
        let original = regions[3].1;
        let ask = |bytes: &[u8]| {
            let patched = [regions[0], regions[1], regions[2], (IORT, bytes)];
            if let Ok(table) = iort(Machine { regions: &patched }, RSDP) {
                assert!(table.nodes().count() <= bytes.len() / 16);
                for rid in 0..0x200 {
                    let _ = table.route(0, rid);
                }
            }
        };
        for at in 36..original.len() {
            for value in [0x00, 0x01, 0x7f, 0x80, 0xff, original[at].wrapping_add(1), original[at] ^ 0x10] {
                let mut bytes = original.to_vec();
                bytes[at] = value;
                reseal(&mut bytes);
                ask(&bytes);
            }
        }
        for len in 36..original.len() as u32 {
            let mut bytes = original.to_vec();
            declare_len(&mut bytes, len);
            ask(&bytes);
        }
    }
}

/// Memory that counts every byte the decoder reads of it.
#[derive(Clone, Copy)]
struct Counted<'a> {
    machine: Machine<'a>,
    reads: &'a Cell<usize>,
}

impl Phys for Counted<'_> {
    fn readable(self, phys: u64, len: usize) -> bool {
        self.machine.readable(phys, len)
    }

    fn byte(self, phys: u64) -> u8 {
        self.reads.set(self.reads.get() + 1);
        Phys::byte(self.machine, phys)
    }
}

/// 499 root complexes of 40 mappings each, every one naming the table's last
/// node: following each reference by a walk of the nodes when the table is
/// opened would read ten million node headers. Opening reads the table for
/// its checksum, each node and each mapping once; a route walks the nodes
/// once to find its root complex and once for each of the two references it
/// may follow.
#[test]
fn opening_a_table_and_routing_a_requester_each_read_it_a_bounded_number_of_times() {
    const ROOTS: u32 = 499;
    const ROOT_LEN: u32 = 36 + 40 * 20;
    let its_at = 48 + ROOTS * ROOT_LEN;
    let mut nodes: Vec<Vec<u8>> = (0..ROOTS)
        .map(|segment| {
            let mappings: Vec<[u32; 5]> = (0..40).map(|i| [i * 16, 15, 0x1000 + i * 16, its_at, 0]).collect();
            root_complex(segment, &mappings)
        })
        .collect();
    nodes.push(its(7));
    let bytes = table(5, &nodes);
    assert_eq!(bytes.len(), its_at as usize + 24);

    let head = rsdp(0x1000, 2, 36);
    let root = xsdt(&[TABLE_AT]);
    let regions: &[(u64, &[u8])] = &[(0x800, &head), (0x1000, &root), (TABLE_AT, &bytes)];
    let reads = Cell::new(0);
    let opened = iort(Counted { machine: Machine { regions }, reads: &reads }, 0x800).expect("a table of 500 nodes");
    let open = reads.replace(0);
    assert!(open <= 3 * bytes.len(), "opening {} bytes read {open}", bytes.len());

    // The last mapping of the last root complex, so nothing is found early.
    let device = 0x1000 + 39 * 16 + 15;
    assert_eq!(opened.route(ROOTS - 1, 39 * 16 + 15), Ok(Route::Untranslated(ItsDevice { its: 7, device })));
    let routed = reads.replace(0);
    assert!(routed <= bytes.len() / 8, "routing through {} bytes read {routed}", bytes.len());
    assert_eq!(opened.route(ROOTS, 0), Ok(Route::Unmapped));

    // Every mapping to one SMMUv3, each onto streams of its own: a stream is
    // judged alone by one more walk of the nodes and their mappings.
    let mut nodes: Vec<Vec<u8>> = vec![smmu(&[])];
    nodes.extend((0..ROOTS).map(|segment| {
        let mappings: Vec<[u32; 5]> =
            (0..40).map(|i| [i * 16, 15, (segment * 40 + i) * 16, 48, 0]).collect();
        root_complex(segment, &mappings)
    }));
    let bytes = table(5, &nodes);
    let regions: &[(u64, &[u8])] = &[(0x800, &head), (0x1000, &root), (TABLE_AT, &bytes)];
    let opened = iort(Counted { machine: Machine { regions }, reads: &reads }, 0x800).expect("a table of 500 nodes");
    reads.set(0);
    let stream = ((ROOTS - 1) * 40 + 39) * 16 + 15;
    assert_eq!(
        opened.route(ROOTS - 1, 39 * 16 + 15),
        Ok(Route::Translated { smmu: plain_smmu(), stream, its: None })
    );
    let routed = reads.get();
    assert!(routed <= 2 * bytes.len(), "routing through {} bytes read {routed}", bytes.len());
}
