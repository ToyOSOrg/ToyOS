//! The IORT and the MADT's MSI controllers: QEMU `virt`'s own tables in both
//! of its GIC modes, decoded against what was read off their bytes by hand
//! with DEN 0049 E.g and ACPI 6.5 open, and every refusal over crafted ones.

mod common;

use core::num::NonZeroU32;

use common::{declare_len, entry, madt, reseal, rsdp, sdt, xsdt, Machine};
use toyos_acpi::{
    find_table, iort, madt_entries, msi_controllers, IortRefused, ItsDevice, MadtEntry, MadtHalt, MsiController,
    MsiFrame, Node, Route, Smmuv3, SpiRange, TableError, MADT_ENTRIES,
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
/// override` set, and the GSIVs in the node's order — event, PRI, GERR, sync —
/// of which GERR is the higher of the last two.
fn virt_smmu() -> Smmuv3 {
    Smmuv3 {
        base: 0x0905_0000,
        coherent: true,
        event: NonZeroU32::new(106),
        pri: NonZeroU32::new(107),
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

fn controllers(regions: &[(u64, &[u8])]) -> Vec<Result<MsiController, MadtHalt>> {
    let table = find_table(Machine { regions }, RSDP, b"APIC", MADT_ENTRIES).expect("MADT");
    msi_controllers(&table).collect()
}

/// `arm-gicv2m` at `0x08020000`, 64 SPIs from 80, named by the structure.
#[test]
fn the_hardware_gic_boot_names_a_v2m_frame_and_no_its() {
    assert_eq!(
        controllers(HWGIC),
        [Ok(MsiController::Frame(MsiFrame {
            id: 0,
            base: 0x0802_0000,
            spis: Some(SpiRange { base: 80, count: 64 }),
        }))]
    );
    // The entry walk a kernel already makes still answers it by its type alone.
    let table = find_table(Machine { regions: HWGIC }, RSDP, b"APIC", MADT_ENTRIES).expect("MADT");
    assert_eq!(madt_entries(&table).last(), Some(Ok(MadtEntry::Other(0xD))));
}

/// `arm-gicv3-its` at `0x08080000`, under the id the IORT's ITS node carries.
#[test]
fn the_its_boot_names_the_its_the_iort_routes_to() {
    assert_eq!(controllers(ITS), [Ok(MsiController::Its { id: 0, base: 0x0808_0000 })]);
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

/// A revision 4 SMMUv3 at `base` with no flag and no GSIV: 52 bytes of fields.
fn smmu(base: u64, mappings: &[[u32; 5]]) -> Vec<u8> {
    let mut fields = vec![0u8; 52];
    fields[..8].copy_from_slice(&base.to_le_bytes());
    node(4, 4, &fields, mappings)
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
    table(5, &[its(7), smmu(0x0905_0000, onward), root_complex(0, root)])
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
    Smmuv3 { base: 0x0905_0000, coherent: false, event: None, pri: None, gerror: None, sync: None }
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
    let mut cut = table(5, &[smmu(0, &[]), its(0)]);
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
        (4, smmu(0, &[]), &[4, 5], &[3, 6]),
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
fn an_output_reference_that_names_no_node_is_refused() {
    // Zero, the table's own header, the middle of a node, one past the last node.
    for reference in [0, 36, SMMU_AT + 4, ROOT_AT + 56, u32::MAX] {
        assert_eq!(
            refusal(&three(&[[0, 0xff, 0, reference, 0]], &[])),
            Some(IortRefused::Output { at: 140, reference }),
            "reference {reference}"
        );
    }
}

#[test]
fn an_output_reference_to_a_node_its_source_may_not_reach_is_refused() {
    // A root complex to a root complex — itself.
    assert_eq!(
        refusal(&three(&[[0, 0xff, 0, ROOT_AT, 0]], &[])),
        Some(IortRefused::Output { at: 140, reference: ROOT_AT })
    );
    // An SMMUv3 to an SMMUv3 — itself — and to a root complex.
    for reference in [SMMU_AT, ROOT_AFTER_ONE] {
        assert_eq!(
            refusal(&three(&[], &[[0, 0xff, 0, reference, 0]])),
            Some(IortRefused::Output { at: 72, reference })
        );
    }
    // A root complex to a node type not decoded: an SMMUv1 or v2.
    let old = table(5, &[node(3, 3, &[0u8; 44], &[]), root_complex(0, &[[0, 0xff, 0, 48, 0]])]);
    assert_eq!(refusal(&old), Some(IortRefused::Output { at: 108, reference: 48 }));
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
fn a_single_mapping_is_the_smmus_own_message_and_never_a_root_complexs() {
    assert_eq!(
        refusal(&three(&[[0, 0xff, 0, SMMU_AT, 1]], &[])),
        Some(IortRefused::SingleMapping { at: 140 })
    );
    // On the SMMUv3 it names the DeviceID of the unit's own interrupts: its
    // input fields are ignored, so it routes no stream and its range is not judged.
    let own = three(&[[0, 0xff, 0, SMMU_AT, 0]], &[[0, 0, 0x30, ITS_AT, 1]]);
    assert_eq!(route(&own, 0, 0), Ok(Route::Translated { smmu: plain_smmu(), stream: 0, its: None }));
    let wide = three(&[[0, 0xff, 0, SMMU_AT, 0]], &[[0xffff_ffff, 0xffff_ffff, 0x30, ITS_AT, 1]]);
    assert_eq!(refusal(&wide), None);
    // It still has to name an ITS.
    assert_eq!(
        refusal(&three(&[], &[[0, 0, 0x30, ROOT_AFTER_ONE, 1]])),
        Some(IortRefused::Output { at: 72, reference: ROOT_AFTER_ONE })
    );
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

// --- the MADT's MSI controllers -------------------------------------------

fn crafted_controllers(list: &[u8]) -> Vec<Result<MsiController, MadtHalt>> {
    let t = madt(list);
    let head = rsdp(0x1000, 2, 36);
    let root = xsdt(&[TABLE_AT]);
    let regions: &[(u64, &[u8])] = &[(0x800, &head), (0x1000, &root), (TABLE_AT, &t)];
    let table = find_table(Machine { regions }, 0x800, b"APIC", MADT_ENTRIES).expect("MADT");
    msi_controllers(&table).take(8).collect()
}

/// A GIC MSI Frame structure's 22 bytes after its type and length.
fn frame(id: u32, base: u64, flags: u32, count: u16, spi_base: u16) -> Vec<u8> {
    let mut body = vec![0u8; 2];
    body.extend(id.to_le_bytes());
    body.extend(base.to_le_bytes());
    body.extend(flags.to_le_bytes());
    body.extend(count.to_le_bytes());
    body.extend(spi_base.to_le_bytes());
    entry(0xD, 24, &body)
}

#[test]
fn a_frame_names_its_spis_only_where_its_flag_says_the_structure_does() {
    let mut list = frame(3, 0x0802_0000, 0, 64, 80);
    list.extend(frame(4, 0x0803_0000, 1, 32, 144));
    // Every flag but the select bit: reserved, and no word about the SPIs.
    list.extend(frame(5, 0x0804_0000, !1, 8, 200));
    assert_eq!(
        crafted_controllers(&list),
        [
            Ok(MsiController::Frame(MsiFrame { id: 3, base: 0x0802_0000, spis: None })),
            Ok(MsiController::Frame(MsiFrame {
                id: 4,
                base: 0x0803_0000,
                spis: Some(SpiRange { base: 144, count: 32 })
            })),
            Ok(MsiController::Frame(MsiFrame { id: 5, base: 0x0804_0000, spis: None })),
        ]
    );
}

#[test]
fn the_controllers_come_in_the_tables_order_past_every_other_structure() {
    // A GICD, an ITS, a frame, a second ITS.
    let mut list = entry(0xC, 24, &[0u8; 22]);
    let its = |id: u32, base: u64| {
        let mut body = vec![0u8; 2];
        body.extend(id.to_le_bytes());
        body.extend(base.to_le_bytes());
        body.extend([0u8; 4]);
        entry(0xF, 20, &body)
    };
    list.extend(its(1, 0x0808_0000));
    list.extend(frame(2, 0x0802_0000, 0, 0, 0));
    list.extend(its(9, 0x0809_0000));
    assert_eq!(
        crafted_controllers(&list),
        [
            Ok(MsiController::Its { id: 1, base: 0x0808_0000 }),
            Ok(MsiController::Frame(MsiFrame { id: 2, base: 0x0802_0000, spis: None })),
            Ok(MsiController::Its { id: 9, base: 0x0809_0000 }),
        ]
    );
}

#[test]
fn a_frame_too_short_for_its_fields_is_no_controller_and_a_halt_ends_the_walk() {
    // Twenty-two bytes: the SPI base would be read from the next structure.
    let mut list = entry(0xD, 22, &[0xff; 20]);
    list.extend(frame(1, 0x0802_0000, 0, 0, 0));
    list.extend(entry(0xD, 0, &[]));
    list.extend(frame(2, 0x0803_0000, 0, 0, 0));
    assert_eq!(
        crafted_controllers(&list),
        [
            Ok(MsiController::Frame(MsiFrame { id: 1, base: 0x0802_0000, spis: None })),
            Err(MadtHalt { at: 46, declared: 0, list_len: 72 }),
        ]
    );
}
