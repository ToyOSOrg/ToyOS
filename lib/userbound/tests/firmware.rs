//! The `acpi` claim's mediated access, row by row: what passes, what each
//! refusal is called, and that the edges of every range fall on the right
//! side.

use toyos_abi::acpi::{Refused, Width};
use toyos_abi::boot::MemoryMapEntry;
use toyos_userbound::firmware::{
    config, lock_word, port, sleep_type, type_word, CallRate, Enumerated, Function, HostBridge, Memory, MemoryVerdict, NoLockWord,
    PortVerdict, Standing, CALLS, CALL_PERIOD_NS, FIXED_RANGE_END,
};
use toyos_acpi::EcamWindow;
use toyos_userbound::{KeptCommands, Mediated};

/// Windows as MCFG allocation structures of segment group 0 name them, each
/// `(base, first bus, last bus)`, through the decode that is the only maker of
/// one.
fn windows(named: &[(u64, u8, u8)]) -> &'static [EcamWindow] {
    let decode = |&(base, first_bus, last_bus): &(u64, u8, u8)| {
        let mut entry = [0u8; 16];
        entry[..8].copy_from_slice(&base.to_le_bytes());
        (entry[10], entry[11]) = (first_bus, last_bus);
        EcamWindow::decode(&entry).expect("a well-formed window")
    };
    named.iter().map(decode).collect::<Vec<_>>().leak()
}

fn ecam(base: u64, first_bus: u8, last_bus: u8) -> &'static [EcamWindow] {
    windows(&[(base, first_bus, last_bus)])
}

const fn e(uefi_type: u32, start: u64, end: u64) -> MemoryMapEntry {
    MemoryMapEntry { uefi_type, start, end }
}

const GIB: u64 = 1 << 30;

/// Descriptors of the map QEMU 11.1.1's edk2 hands a 2 GiB q35 guest
/// (`lib/bootmap/tests/direct_map.rs` holds it whole), with an ACPI reclaim
/// range where that boot's tables were: RAM, ACPI NVS beside RAM, the ECAM
/// window typed reserved, and a reserved range above everything mapped.
const Q35: [MemoryMapEntry; 9] = [
    e(7, 0x0, 0x87000),
    e(4, 0x87000, 0x88000),
    e(7, 0x100000, 0x800000),
    e(10, 0x800000, 0x808000),
    e(7, 0x808000, 0x80b000),
    e(9, 0x7fb74000, 0x7fb7f000),
    e(10, 0x7ff60000, 0x80000000),
    e(0, 0xe0000000, 0xf0000000),
    e(0, 0xfd00000000, 0x10000000000),
];
/// The I/O APIC and the HPET, as the kernel maps them on q35.
const Q35_DRIVEN: &[(u64, u64)] = &[(0xfec0_0000, 0xfec0_0020), (0xfed0_0000, 0xfed0_1000)];
const Q35_FACS: (u64, u64) = (0x7ff7_7000, 0x7ff7_7040);
/// q35's MCH, which 631122 does not cover, with a window at 48h all the same.
const Q35_HOST_BRIDGE: HostBridge = HostBridge { vendor: 0x8086, device: 0x29C0, at_48h: 0xfed1_0001 };

/// The ranges devices decode, as a test lists them.
type Devices = std::iter::Copied<std::slice::Iter<'static, (u64, u64)>>;
type Mem = Memory<'static, Devices>;

fn devices(decoded: &'static [(u64, u64)]) -> Devices {
    decoded.iter().copied()
}

/// A machine of one map and nothing else: no ECAM window, no FACS.
fn bare(map: &'static [MemoryMapEntry], decoded: &'static [(u64, u64)]) -> Mem {
    Memory { map, mapped_end: 4 * GIB, ecam: &[], devices: devices(decoded), facs: None, uncached: registers, registers_differ: false, host_bridge: HostBridge::ABSENT }
}

/// What the range registers type uncacheable on these machines: the hole
/// under 4 GiB from [`REGISTERS`] up, where a chipset keeps its registers.
const REGISTERS: u64 = 0xfe00_0000;

fn registers(at: u64, len: u64) -> bool {
    at >= REGISTERS && at + len <= 4 * GIB
}

fn q35() -> Mem {
    Memory { map: &Q35, mapped_end: 4 * GIB, ecam: ecam(0xe000_0000, 0, 0xFF), devices: devices(Q35_DRIVEN), facs: Some(Q35_FACS), uncached: registers, registers_differ: false, host_bridge: Q35_HOST_BRIDGE }
}

/// A map shaped as a laptop's is, at addresses of this test's own: RAM, a
/// reserved range, ACPI NVS, ACPI reclaim, a page of RAM after them, runtime
/// services data, a memory-mapped I/O range, and the ECAM window typed as
/// memory-mapped I/O.
const LAPTOP: [MemoryMapEntry; 9] = [
    e(7, 0x0, 0x9f000),
    e(0, 0x9f000, 0x100000),
    e(7, 0x100000, 0x7000_0000),
    e(0, 0x7000_0000, 0x7400_0000),
    e(10, 0x7400_0000, 0x7480_0000),
    e(9, 0x7480_0000, 0x7490_0000),
    e(7, 0x7490_0000, 0x7490_1000),
    e(6, 0x7490_1000, 0x74a0_0000),
    e(11, 0xc000_0000, 0xd000_0000),
];

fn laptop() -> Mem {
    Memory { map: &LAPTOP, mapped_end: 4 * GIB, ecam: ecam(0xc000_0000, 0, 0xFF), devices: devices(&[]), facs: Some((0x7400_0040, 0x7400_0080)), uncached: registers, registers_differ: false, host_bridge: HostBridge::ABSENT }
}

fn passes(memory: &Mem, at: u64, width: Width, write: bool) -> bool {
    match memory.clone().decide(at, width, write) {
        MemoryVerdict::Through(witness) => {
            assert_eq!((witness.at(), witness.width()), (at, width), "the witness names another access");
            true
        }
        _ => false,
    }
}

fn refused(memory: &Mem, at: u64, width: Width, write: bool) -> Refused {
    match memory.clone().decide(at, width, write) {
        MemoryVerdict::Refused(why) => why,
        other => panic!("{at:#x} {width:?} write={write} was not refused: {other:?}"),
    }
}

#[test]
fn ram_is_refused_both_ways_whatever_usable_type_it_is() {
    let q35 = q35();
    for write in [false, true] {
        for width in [Width::Byte, Width::Word, Width::DWord, Width::QWord] {
            // Conventional memory, boot services data, and the last byte of each.
            for at in [0x0, 0x100000, 0x87000, 0x808000, 0x800000 - width.bytes()] {
                assert_eq!(refused(&q35, at, width, write), Refused::UsableMemory, "{at:#x} {width:?}");
            }
        }
    }
    assert_eq!(type_word(&Q35, 0x100000), 7);
    assert_eq!(type_word(&Q35, 0x87000), 4);
}

#[test]
fn nvs_and_reserved_memory_pass_both_ways_to_their_last_byte_and_no_further() {
    let (q35, laptop) = (q35(), laptop());
    for write in [false, true] {
        assert!(passes(&q35, 0x800000, Width::QWord, write));
        assert!(passes(&q35, 0x808000 - 8, Width::QWord, write));
        assert!(passes(&q35, 0x7ff60000, Width::Byte, write));
        assert!(passes(&laptop, 0x9f000, Width::DWord, write), "reserved memory below 1 MiB");
        assert!(passes(&laptop, 0x7000_0000, Width::Word, write));
        // One byte out of the range is in RAM, whichever end it leaves by.
        assert_eq!(refused(&q35, 0x808000 - 7, Width::QWord, write), Refused::Straddles);
        assert_eq!(refused(&q35, 0x800000 - 1, Width::Word, write), Refused::Straddles);
        assert_eq!(refused(&q35, 0x808000, Width::Byte, write), Refused::UsableMemory);
    }
    assert_eq!(type_word(&Q35, 0x800000), 10);
    assert_eq!(type_word(&LAPTOP, 0x7000_0000), 0);
}

#[test]
fn the_tables_memory_is_read_and_never_written() {
    let (q35, laptop) = (q35(), laptop());
    for memory in [&q35, &laptop] {
        let reclaim = memory.map.iter().find(|entry| entry.uefi_type == 9).expect("a reclaim range");
        assert!(passes(memory, reclaim.start, Width::QWord, false));
        assert!(passes(memory, reclaim.end - 1, Width::Byte, false));
        assert_eq!(refused(memory, reclaim.start, Width::QWord, true), Refused::TableWrite);
        assert_eq!(refused(memory, reclaim.end - 1, Width::Byte, true), Refused::TableWrite);
    }
    // Reclaim beside NVS: a read across the two is still two types.
    assert_eq!(refused(&laptop, 0x7480_0000 - 2, Width::DWord, false), Refused::Straddles);
}

/// A firmware that keeps the tables its XSDT lists in runtime-services data,
/// as one real machine's does: read to its last byte, never written, and a
/// read that leaves it for the RAM beside it is refused.
#[test]
fn runtime_services_data_is_read_as_the_tables_memory_is_and_never_written() {
    let laptop = laptop();
    let data = LAPTOP.iter().find(|entry| entry.uefi_type == 6).expect("a runtime-services data range");
    for width in [Width::Byte, Width::QWord] {
        assert!(passes(&laptop, data.start, width, false));
        assert!(passes(&laptop, data.end - width.bytes(), width, false));
        assert_eq!(refused(&laptop, data.start, width, true), Refused::TableWrite);
        assert_eq!(refused(&laptop, data.end - width.bytes(), width, true), Refused::TableWrite);
    }
    assert_eq!(refused(&laptop, data.start - 1, Width::Word, false), Refused::Straddles);
    assert_eq!(refused(&laptop, data.start - 1, Width::Byte, false), Refused::UsableMemory);
    assert_eq!(type_word(&LAPTOP, data.start), 6);
    // A device's page inside it is still a device's.
    let decoding = bare(&LAPTOP, &[(0x7495_0000, 0x7495_0100)]);
    assert_eq!(refused(&decoding, 0x7495_0800, Width::Byte, false), Refused::DeviceMemory);
    assert!(passes(&decoding, 0x7495_1000, Width::Byte, false));
}

/// An address the map does not list is a register where the range registers
/// type it uncacheable, inside what the kernel maps and at or above 1 MiB,
/// and it is written where it is read: in each width, the T14's pad
/// configuration byte at 0xFD6E0A70 among them. What the kernel drives or a
/// function decodes there is a device's still, and the ECAM window a
/// configuration access.
#[test]
fn an_unlisted_register_is_written_where_it_is_read() {
    let laptop = laptop();
    const AT: u64 = REGISTERS + 0x12_3000;
    assert_eq!(type_word(&LAPTOP, AT), toyos_abi::acpi::UNLISTED);
    let t14 = t14();
    for write in [false, true] {
        for width in [Width::Byte, Width::Word, Width::DWord, Width::QWord] {
            assert!(passes(&laptop, AT + 0x110, width, write), "{width:?} write={write}");
        }
        assert!(passes(&t14, 0xFD6E_0A70, Width::Byte, write), "the T14's GPPC_A_13 pad");
        // The last byte the range registers type so, and the first they do not.
        assert!(passes(&laptop, REGISTERS, Width::Byte, write));
        assert_eq!(refused(&laptop, REGISTERS - 1, Width::Byte, write), Refused::RegisterCached);
        assert_eq!(refused(&laptop, REGISTERS - 1, Width::Word, write), Refused::RegisterCached, "an access that begins outside it");
        assert!(passes(&laptop, 4 * GIB - 9, Width::QWord, write));
        // Past what the kernel maps there is nothing to reach it through.
        assert_eq!(refused(&laptop, 4 * GIB - 4, Width::QWord, write), Refused::Unmapped);
        let low = Memory { mapped_end: AT + 0x110, ..bare(&LAPTOP, &[]) };
        assert_eq!(refused(&low, AT + 0x110, Width::Byte, write), Refused::Unmapped);
        assert!(passes(&low, AT + 0x10f, Width::Byte, write));
        // A page the kernel knows a device decodes in, a driven window's and
        // a BAR's alike, and the local APIC's.
        let decoding = bare(&LAPTOP, &[(AT, AT + 0x20)]);
        assert_eq!(refused(&decoding, AT + 0x110, Width::Byte, write), Refused::DeviceMemory);
        assert_eq!(refused(&q35(), 0xfee0_0000, Width::DWord, write), Refused::DeviceMemory, "the local APIC");
        assert_eq!(refused(&t14, 0xFED9_0000, Width::DWord, write), Refused::DeviceMemory, "a VT-d unit the kernel drives");
    }
    // Listed memory is decided by its type, whatever the range registers say of it.
    const TYPED: [MemoryMapEntry; 3] = [e(7, REGISTERS, REGISTERS + 0x1000), e(5, REGISTERS + 0x1000, REGISTERS + 0x2000), e(0, REGISTERS + 0x2000, REGISTERS + 0x3000)];
    let typed = bare(&TYPED, &[]);
    assert_eq!(refused(&typed, REGISTERS, Width::Byte, true), Refused::UsableMemory);
    assert_eq!(refused(&typed, REGISTERS + 0x1000, Width::Byte, true), Refused::MemoryType);
    assert!(passes(&typed, REGISTERS + 0x2000, Width::Byte, true));
    // An access across listed memory and a hole is two things.
    assert_eq!(refused(&typed, REGISTERS + 0x3000 - 1, Width::Word, true), Refused::Straddles);
}

/// Memory the firmware's map types `EfiMemoryMappedIO` is a device's
/// registers, as the AMD bench's DSDT reads one while it loads: read and
/// written, in each width, exactly where an unlisted register is (inside the
/// direct map, at or above 1 MiB, typed uncacheable by the range registers
/// every CPU holds). A page the kernel knows a device decodes in, the local
/// APIC's and an ECAM window typed so are what they are whatever the map
/// says, and an access that leaves the range for RAM is refused.
#[test]
fn memory_mapped_io_is_accessed_as_a_register_is() {
    const AT: u64 = REGISTERS + 0x10_0000;
    const MMIO: [MemoryMapEntry; 4] = [
        e(7, 0x0, 0x8000_0000),
        e(11, 0x8000_0000, 0x8010_0000),
        e(7, AT - 0x1000, AT),
        e(11, AT, AT + 0x10_0000),
    ];
    let memory = bare(&MMIO, &[]);
    assert_eq!(type_word(&MMIO, AT), 11);
    for write in [false, true] {
        for width in [Width::Byte, Width::Word, Width::DWord, Width::QWord] {
            assert!(passes(&memory, AT + 0x110, width, write), "{width:?}");
            assert!(passes(&memory, AT + 0x10_0000 - width.bytes(), width, write), "{width:?} at its last byte");
        }
        // Typed memory-mapped I/O the range registers cache is no register.
        assert_eq!(refused(&memory, 0x8000_0000, Width::Byte, write), Refused::RegisterCached);
    }
    // A read across it and the RAM beside it, from either side.
    assert_eq!(refused(&memory, AT - 1, Width::Word, false), Refused::Straddles);
    assert_eq!(refused(&memory, AT - 1, Width::Byte, false), Refused::UsableMemory);
    // Some CPU's range registers on and not the boot processor's.
    let differing = Memory { registers_differ: true, ..bare(&MMIO, &[]) };
    assert_eq!(refused(&differing, AT + 0x110, Width::DWord, false), Refused::RangeRegistersDiffer);
    // Past what the kernel maps there is nothing to read it through.
    let low = Memory { mapped_end: AT + 0x110, ..bare(&MMIO, &[]) };
    assert_eq!(refused(&low, AT + 0x110, Width::Byte, false), Refused::Unmapped);
    // A page a driven window or a function's BAR holds, and the local APIC.
    let decoding = bare(&MMIO, &[(AT + 0x2000, AT + 0x2004)]);
    assert_eq!(refused(&decoding, AT + 0x2ff8, Width::QWord, false), Refused::DeviceMemory);
    assert!(passes(&decoding, AT + 0x3000, Width::QWord, false));
    const APIC: [MemoryMapEntry; 1] = [e(11, 0xfee0_0000, 0xfef0_0000)];
    assert_eq!(refused(&bare(&APIC, &[]), 0xfee0_0030, Width::DWord, false), Refused::DeviceMemory);
    // An ECAM window the map types memory-mapped I/O is configuration space.
    assert!(matches!(laptop().decide(0xc000_8000, Width::DWord, false), MemoryVerdict::AsConfig(..)));
}

/// The range registers the cache check answers from are the boot processor's.
/// Where some CPU's are on and are not those, no address the map does not
/// list is read or written, a register's and one below 1 MiB alike; listed
/// memory, which its type decides, and every other refusal are as they were.
#[test]
fn no_unlisted_address_is_accessed_where_a_cpus_range_registers_differ() {
    let agreeing = laptop();
    let differing = Memory { registers_differ: true, ..laptop() };
    const AT: u64 = REGISTERS + 0x12_3110;
    for write in [false, true] {
        for width in [Width::Byte, Width::Word, Width::DWord, Width::QWord] {
            assert!(passes(&agreeing, AT, width, write), "{width:?}");
            assert_eq!(refused(&differing, AT, width, write), Refused::RangeRegistersDiffer, "{width:?}");
        }
    }
    assert_eq!(refused(&differing, REGISTERS - 1, Width::Byte, false), Refused::RangeRegistersDiffer);
    const HOLE: &[MemoryMapEntry] = &[e(7, 0x0, 0xa_0000)];
    let low = Memory { registers_differ: true, ..bare(HOLE, &[]) };
    assert_eq!(refused(&low, 0xa_0000, Width::Byte, false), Refused::RangeRegistersDiffer);
    // What the map lists, a device decodes or the kernel does not map.
    assert!(passes(&differing, 0x7000_0000, Width::DWord, true), "reserved memory");
    assert!(passes(&differing, 0x7400_0000, Width::DWord, false), "ACPI NVS");
    assert_eq!(refused(&differing, 0x10_0000, Width::Byte, false), Refused::UsableMemory);
    assert_eq!(refused(&differing, 4 * GIB - 4, Width::QWord, false), Refused::Unmapped);
    let decoding = Memory { registers_differ: true, ..bare(&LAPTOP, &[(AT, AT + 0x20)]) };
    assert_eq!(refused(&decoding, AT, Width::Byte, false), Refused::DeviceMemory);
}

/// Below 1 MiB the fixed range registers decide what is cached, and the
/// kernel reads none of them: an unlisted address there is refused whatever
/// the cache check answers, to the last byte below the bound.
#[test]
fn an_unlisted_address_below_1_mib_is_refused_whatever_the_range_registers_answer() {
    // RAM, and a hole from the legacy video memory up that the map never lists.
    const HOLE: &[MemoryMapEntry] = &[e(7, 0x0, 0xa_0000)];
    let memory = Memory { uncached: |_, _| true, ..bare(HOLE, &[]) };
    for width in [Width::Byte, Width::Word, Width::DWord, Width::QWord] {
        for at in [0xa_0000, 0xc_0000, FIXED_RANGE_END - 8, FIXED_RANGE_END - width.bytes()] {
            assert_eq!(refused(&memory, at, width, false), Refused::RegisterCached, "{at:#x} {width:?}");
            assert_eq!(refused(&memory, at, width, true), Refused::RegisterCached, "{at:#x} {width:?}");
        }
        assert!(passes(&memory, FIXED_RANGE_END, width, false), "{width:?} at the bound");
        assert!(passes(&memory, FIXED_RANGE_END, width, true), "{width:?} at the bound");
    }
    assert_eq!(refused(&memory, FIXED_RANGE_END - 1, Width::Byte, false), Refused::RegisterCached);
    assert_eq!(refused(&memory, FIXED_RANGE_END - 1, Width::Word, false), Refused::RegisterCached, "a read that begins below the bound");
    // Listed memory below the bound is decided by its type, as anywhere.
    assert!(passes(&laptop(), 0x9f000, Width::DWord, false));
}

/// A map that lists a range twice, or two ranges over one another: the
/// allocator takes every usable range, so a byte any usable range holds is
/// RAM, whichever range lists it first.
#[test]
fn memory_any_usable_range_holds_is_refused_whatever_lists_it_first() {
    for firmware in [0, 6, 9, 10] {
        for handed_out in [1, 2, 3, 4, 7] {
            // The same range under both types, the firmware's first.
            let twice = [e(firmware, 0x1000, 0x3000), e(handed_out, 0x1000, 0x3000)];
            // A usable range over the firmware range's second page and beyond.
            let over = [e(firmware, 0x1000, 0x3000), e(handed_out, 0x2000, 0x4000)];
            for write in [false, true] {
                let twice = Memory { map: &twice, mapped_end: 4 * GIB, ecam: &[], devices: devices(&[]), facs: None, uncached: registers, registers_differ: false, host_bridge: HostBridge::ABSENT };
                let over = Memory { map: &over, mapped_end: 4 * GIB, ecam: &[], devices: devices(&[]), facs: None, uncached: registers, registers_differ: false, host_bridge: HostBridge::ABSENT };
                let why = MemoryVerdict::Refused(Refused::UsableMemory);
                assert_eq!(twice.clone().decide(0x1000, Width::Byte, write), why, "type {firmware} listed before {handed_out}");
                assert_eq!(twice.decide(0x2ff8, Width::QWord, write), why);
                assert_eq!(over.clone().decide(0x2000, Width::Byte, write), why, "type {firmware} under {handed_out}");
                assert_eq!(over.clone().decide(0x1ffc, Width::QWord, write), why, "an access whose last bytes a usable range holds");
                assert_eq!(over.clone().decide(0x2fff, Width::Byte, write), why);
                // The page no usable range holds is the firmware's still.
                let alone = over.decide(0x1ff8, Width::QWord, write);
                match (firmware, write) {
                    (6 | 9, true) => assert_eq!(alone, MemoryVerdict::Refused(Refused::TableWrite)),
                    _ => assert!(matches!(alone, MemoryVerdict::Through(_)), "type {firmware} write={write}: {alone:?}"),
                }
            }
        }
    }
    // The lock word is exchanged in no byte a usable range holds either.
    assert_eq!(lock_word(&[e(10, 0x1000, 0x2000), e(4, 0x1000, 0x2000)], 4 * GIB, 0x1010), Err(NoLockWord::Type(Some(4))));
    assert_eq!(lock_word(&[e(10, 0x1000, 0x2000), e(7, 0x1013, 0x2000)], 4 * GIB, 0x1010), Err(NoLockWord::Type(Some(7))));
    assert!(lock_word(&[e(10, 0x1000, 0x2000), e(7, 0x1014, 0x2000)], 4 * GIB, 0x1010).is_ok());
}

/// Runtime-services code is the firmware's to execute and nobody's to read
/// through this claim: refused both ways, with its type.
#[test]
fn runtime_services_code_is_refused_both_ways() {
    const CODE: [MemoryMapEntry; 2] = [e(6, 0x7490_1000, 0x74a0_0000), e(5, 0x74a0_0000, 0x74b0_0000)];
    let memory = bare(&CODE, &[]);
    for write in [false, true] {
        assert_eq!(refused(&memory, 0x74a0_0000, Width::Byte, write), Refused::MemoryType);
        assert_eq!(refused(&memory, 0x74b0_0000 - 8, Width::QWord, write), Refused::MemoryType);
    }
    assert_eq!(type_word(&CODE, 0x74a0_0000), 5);
    // Data beside code: a read across the two is two types.
    assert_eq!(refused(&memory, 0x74a0_0000 - 4, Width::QWord, false), Refused::Straddles);
}

#[test]
fn every_other_type_and_an_unlisted_address_is_refused_with_its_type() {
    let laptop = laptop();
    // A hole the map does not list, which the range registers cache: no
    // register is there.
    assert_eq!(refused(&laptop, 0x8000_0000, Width::Byte, true), Refused::RegisterCached);
    assert_eq!(refused(&laptop, 0x8000_0000, Width::Byte, false), Refused::RegisterCached);
    assert_eq!(type_word(&LAPTOP, 0x8000_0000), toyos_abi::acpi::UNLISTED);
    // Every type but the four the policy names, as the only range of a map.
    for ty in (0..=0x20u32).chain([0x7000_0000, 0x8000_0000, u32::MAX]) {
        let map = [e(ty, 0x1000, 0x2000)];
        let memory = Memory { map: &map, mapped_end: 4 * GIB, ecam: &[], devices: devices(&[]), facs: None, uncached: registers, registers_differ: false, host_bridge: HostBridge::ABSENT };
        let read = memory.clone().decide(0x1000, Width::Byte, false);
        let write = memory.decide(0x1000, Width::Byte, true);
        let through = |verdict| matches!(verdict, MemoryVerdict::Through(_));
        match ty {
            0 | 10 => assert!(through(read) && through(write), "type {ty}"),
            6 | 9 => assert!(through(read) && write == MemoryVerdict::Refused(Refused::TableWrite), "type {ty}"),
            // Below 1 MiB, where no register is read.
            11 => assert!(read == write && read == MemoryVerdict::Refused(Refused::RegisterCached), "type {ty}"),
            1 | 2 | 3 | 4 | 7 => assert!(read == write && read == MemoryVerdict::Refused(Refused::UsableMemory), "type {ty}"),
            _ => assert!(read == write && read == MemoryVerdict::Refused(Refused::MemoryType), "type {ty}"),
        }
    }
}

#[test]
fn memory_past_what_the_kernel_maps_is_refused() {
    let q35 = q35();
    assert_eq!(refused(&q35, 0xfd_0000_0000, Width::Byte, false), Refused::Unmapped);
    const ACROSS_THE_END: &[MemoryMapEntry] = &[e(0, 4 * GIB - 0x1000, 4 * GIB + 0x1000)];
    let memory = bare(ACROSS_THE_END, &[]);
    assert!(passes(&memory, 4 * GIB - 8, Width::QWord, true));
    assert_eq!(refused(&memory, 4 * GIB - 7, Width::QWord, false), Refused::Unmapped);
    assert_eq!(refused(&memory, 4 * GIB, Width::Byte, false), Refused::Unmapped);
    // An access that would wrap the address space names no memory.
    assert_eq!(refused(&q35, u64::MAX, Width::Word, false), Refused::Unmapped);
    assert_eq!(refused(&q35, u64::MAX - 6, Width::QWord, true), Refused::Unmapped);
}

/// The I/O APIC, the HPET and the local APIC, typed reserved by this map.
const DEVICES_RESERVED: &[MemoryMapEntry] = &[e(0, 0xfe00_0000, 0xff00_0000)];

#[test]
fn every_page_a_window_the_kernel_drives_lies_in_is_refused_inside_any_type() {
    let memory = bare(DEVICES_RESERVED, Q35_DRIVEN);
    for write in [false, true] {
        assert_eq!(refused(&memory, 0xfec0_0000, Width::DWord, write), Refused::DeviceMemory);
        assert_eq!(refused(&memory, 0xfec0_001f, Width::Byte, write), Refused::DeviceMemory);
        // The I/O APIC is mapped as 0x20 bytes; its EOI register is at 0x40 of
        // the page, and the page is the device's to its last byte.
        assert_eq!(refused(&memory, 0xfec0_0020, Width::DWord, write), Refused::DeviceMemory, "the dword after the mapped bytes");
        assert_eq!(refused(&memory, 0xfec0_0040, Width::DWord, write), Refused::DeviceMemory, "the EOI register");
        assert_eq!(refused(&memory, 0xfec0_0fff, Width::Byte, write), Refused::DeviceMemory, "the page's last byte");
        assert_eq!(refused(&memory, 0xfec0_0000 - 1, Width::Word, write), Refused::DeviceMemory, "a word that ends in the page");
        assert_eq!(refused(&memory, 0xfec0_0ffd, Width::QWord, write), Refused::DeviceMemory, "a qword that begins in it");
        assert_eq!(refused(&memory, 0xfed0_0ff8, Width::QWord, write), Refused::DeviceMemory);
        assert_eq!(refused(&memory, 0xfed0_0000 - 1, Width::Word, write), Refused::DeviceMemory);
        assert_eq!(refused(&memory, 0xfee0_0000, Width::DWord, write), Refused::DeviceMemory);
        assert_eq!(refused(&memory, 0xfeef_ffff, Width::Byte, write), Refused::DeviceMemory);
        assert_eq!(refused(&memory, 0xfee0_0000 - 4, Width::QWord, write), Refused::DeviceMemory);
        // The pages either side of each device are the firmware's.
        assert!(passes(&memory, 0xfec0_0000 - 8, Width::QWord, write));
        assert!(passes(&memory, 0xfec0_1000, Width::Byte, write));
        assert!(passes(&memory, 0xfed0_0000 - 1, Width::Byte, write));
        assert!(passes(&memory, 0xfed0_1000, Width::Byte, write));
        assert!(passes(&memory, 0xfee0_0000 - 8, Width::QWord, write));
        assert!(passes(&memory, 0xfef0_0000, Width::Byte, write));
    }
    // A window that begins and ends inside pages takes each page it touches.
    let memory = bare(DEVICES_RESERVED, &[(0xfe40_0ff0, 0xfe40_1010)]);
    assert_eq!(refused(&memory, 0xfe40_0000, Width::Byte, true), Refused::DeviceMemory);
    assert_eq!(refused(&memory, 0xfe40_1fff, Width::Byte, true), Refused::DeviceMemory);
    assert!(passes(&memory, 0xfe40_0000 - 1, Width::Byte, true));
    assert!(passes(&memory, 0xfe40_2000, Width::Byte, true));
}

#[test]
fn a_memory_bar_is_refused_where_firmware_types_its_range_as_its_own() {
    // A 16 KiB BAR, and a BAR that answered no size and is recorded as one
    // byte, in memory this map types reserved.
    const BARS: &[(u64, u64)] = &[(0xfe10_0000, 0xfe10_4000), (0xfe20_0000, 0xfe20_0001)];
    // The same 16 KiB BAR in ACPI NVS, and one above everything mapped.
    const NVS: &[MemoryMapEntry] = &[e(10, 0x7400_0000, 0x7480_0000), e(0, 0x40_0000_0000, 0x40_1000_0000)];
    const NVS_BARS: &[(u64, u64)] = &[(0x7410_0000, 0x7410_4000), (0x40_0000_0000, 0x40_0100_0000)];
    let reserved = bare(DEVICES_RESERVED, BARS);
    let nvs = bare(NVS, NVS_BARS);
    for write in [false, true] {
        assert_eq!(refused(&reserved, 0xfe10_0000, Width::DWord, write), Refused::DeviceMemory);
        assert_eq!(refused(&reserved, 0xfe10_3fff, Width::Byte, write), Refused::DeviceMemory);
        assert_eq!(refused(&reserved, 0xfe10_0000 - 4, Width::QWord, write), Refused::DeviceMemory);
        assert!(passes(&reserved, 0xfe10_4000, Width::Byte, write), "the byte after the BAR");
        assert!(passes(&reserved, 0xfe10_0000 - 8, Width::QWord, write), "the qword before it");
        assert_eq!(refused(&reserved, 0xfe20_0000, Width::Byte, write), Refused::DeviceMemory);
        assert_eq!(refused(&reserved, 0xfe20_0fff, Width::Byte, write), Refused::DeviceMemory, "the page of a BAR of unknown size");
        assert!(passes(&reserved, 0xfe20_1000, Width::Byte, write));
        assert_eq!(refused(&nvs, 0x7410_0000, Width::QWord, write), Refused::DeviceMemory);
        assert!(passes(&nvs, 0x7410_4000, Width::QWord, write));
        // A device's memory is refused as a device's even where nothing maps it.
        assert_eq!(refused(&nvs, 0x40_0000_0000, Width::DWord, write), Refused::DeviceMemory);
        assert_eq!(refused(&nvs, 0x40_0100_0000, Width::DWord, write), Refused::Unmapped);
    }
    // With no record of the BAR the same addresses pass: the record is what refuses them.
    assert!(passes(&bare(DEVICES_RESERVED, &[]), 0xfe10_0000, Width::DWord, true));
}

#[test]
fn the_facs_is_read_and_its_bytes_are_never_written() {
    let q35 = q35();
    assert!(passes(&q35, Q35_FACS.0 + 16, Width::DWord, false), "the lock word reads");
    assert_eq!(refused(&q35, Q35_FACS.0 + 16, Width::DWord, true), Refused::FacsWrite);
    assert_eq!(refused(&q35, Q35_FACS.0 - 4, Width::QWord, true), Refused::FacsWrite, "a write that ends in it");
    assert_eq!(refused(&q35, Q35_FACS.1 - 1, Width::Byte, true), Refused::FacsWrite);
    assert!(passes(&q35, Q35_FACS.1, Width::Byte, true), "the byte after it is plain NVS");
    assert!(passes(&q35, Q35_FACS.0 - 8, Width::QWord, true));
}

#[test]
fn an_address_in_the_ecam_window_is_a_configuration_access_whatever_the_map_types_it() {
    // Typed reserved on q35 and memory-mapped I/O on the laptop's shape.
    for memory in [q35(), laptop()] {
        let base = memory.ecam[0].base();
        for write in [false, true] {
            assert_eq!(
                memory.clone().decide(base + (3 << 20 | 0x1c << 15 | 5 << 12 | 0x48), Width::DWord, write),
                MemoryVerdict::AsConfig(Function { bus: 3, device: 0x1c, function: 5 }, 0x48)
            );
            assert_eq!(
                memory.clone().decide(base + (0xFF << 20 | 0x1f << 15 | 7 << 12 | 0xFFF), Width::Byte, write),
                MemoryVerdict::AsConfig(Function { bus: 0xFF, device: 0x1f, function: 7 }, 0xFFF),
                "the window's last byte"
            );
            assert_eq!(refused(&memory, base - 1, Width::Word, write), Refused::Straddles);
            assert_eq!(refused(&memory, base + (0x100 << 20) - 1, Width::Word, write), Refused::Straddles);
        }
    }
    // A window that begins at a later bus holds nothing below it.
    const WINDOW: &[MemoryMapEntry] = &[e(0, 0xe000_0000, 0xf000_0000)];
    let memory = Memory { ecam: ecam(0xe000_0000, 0x10, 0x1F), ..bare(WINDOW, &[]) };
    assert!(passes(&memory, 0xe000_0000, Width::Byte, false), "below the first bus is plain reserved memory");
    assert_eq!(memory.clone().decide(0xe100_0000, Width::Byte, false), MemoryVerdict::AsConfig(Function { bus: 0x10, device: 0, function: 0 }, 0));
    assert!(passes(&memory, 0xe200_0000, Width::Byte, false), "past the last bus too");
    // A window ending on the address space's last byte decides its last byte
    // without overflow.
    let top = Memory { ecam: ecam(0xffff_ffff_fff0_0000, 0, 0), ..bare(WINDOW, &[]) };
    assert_eq!(top.clone().decide(u64::MAX, Width::Byte, false), MemoryVerdict::AsConfig(Function { bus: 0, device: 0x1f, function: 7 }, 0xFFF));
    assert_eq!(refused(&top, u64::MAX, Width::Word, false), Refused::Unmapped);
}

/// Every window is read for every access, the first no more than a later one:
/// an address in the second is a configuration access with its own bus,
/// decided as one whatever the map types it, and a read reaches its buses.
#[test]
fn a_later_window_is_held_as_the_first_is() {
    // Buses 0x40..=0x7f at their own base, after the laptop's 0..=0x3f; both
    // typed reserved, as a plain write would pass.
    const SECOND: u64 = 0xd000_0000;
    const WINDOWS: &[MemoryMapEntry] = &[e(0, 0xc000_0000, 0xe000_0000)];
    let memory = Memory { ecam: windows(&[(0xc000_0000, 0, 0x3f), (SECOND, 0x40, 0x7f)]), ..bare(WINDOWS, &[]) };
    let function = Function { bus: 0x42, device: 3, function: 1 };
    let at = SECOND + (0x42 << 20 | 3 << 15 | 1 << 12 | 0x10);
    for write in [false, true] {
        let verdict = memory.clone().decide(at, Width::DWord, write);
        assert_eq!(verdict, MemoryVerdict::AsConfig(function, 0x10), "write={write}");
        // What the kernel does with that verdict.
        let MemoryVerdict::AsConfig(function, offset) = verdict else { unreachable!() };
        let made = config(memory.ecam, 0, function, offset, Width::DWord).and_then(|at| if write { at.write(None).map(|passed| passed.at()) } else { Ok(at) });
        assert_eq!(made.map(|at| (at.function(), at.offset())), if write { Err(Refused::AddressMap) } else { Ok((function, 0x10)) }, "write={write}");
    }
    for bus in [0x3f, 0x40, 0x7f] {
        let function = Function { bus, device: 0, function: 0 };
        assert!(config(memory.ecam, 0, function, 0, Width::DWord).is_ok(), "bus {bus:#x}");
    }
    assert_eq!(config(memory.ecam, 0, Function { bus: 0x80, device: 0, function: 0 }, 0, Width::DWord), Err(Refused::ConfigUnreachable));
}

#[test]
fn the_lock_word_is_exchanged_only_where_all_four_bytes_are_the_firmwares_own() {
    // The FACS of the q35 boot and of the laptop's shape, both in ACPI NVS.
    assert_eq!(lock_word(&Q35, 4 * GIB, Q35_FACS.0 + 16).map(|word| word.at()), Ok(Q35_FACS.0 + 16));
    assert_eq!(lock_word(&LAPTOP, 4 * GIB, 0x7400_0050).map(|word| word.at()), Ok(0x7400_0050));
    assert!(lock_word(&LAPTOP, 4 * GIB, 0x7000_0010).is_ok(), "reserved memory is the firmware's too");

    // A firmware range that ends inside the word, at its last byte, and before
    // it: what follows is RAM, and a word with one byte there is refused.
    for (end, verdict) in [(0x1014, Ok(0x1010)), (0x1013, Err(NoLockWord::Type(Some(7)))), (0x1011, Err(NoLockWord::Type(Some(7)))), (0x1010, Err(NoLockWord::Type(Some(7))))] {
        let map = [e(10, 0x1000, end), e(7, end, 0x2000)];
        assert_eq!(lock_word(&map, 4 * GIB, 0x1010).map(|word| word.at()), verdict, "the firmware's range ends at {end:#x}");
    }
    // The same where nothing is listed after the range, and where the word's
    // first byte is in RAM and its last in the firmware's.
    assert_eq!(lock_word(&[e(10, 0x1000, 0x1012)], 4 * GIB, 0x1010), Err(NoLockWord::Type(None)));
    assert_eq!(lock_word(&[e(7, 0x1000, 0x1012), e(10, 0x1012, 0x2000)], 4 * GIB, 0x1010), Err(NoLockWord::Type(Some(7))));
    assert_eq!(lock_word(&[], 4 * GIB, 0x1010), Err(NoLockWord::Type(None)));

    // Only ACPI NVS and reserved memory: not the tables' memory, which is
    // never written, nor any other type.
    for ty in (0..=0x20u32).chain([0x7000_0000, u32::MAX]) {
        let verdict = lock_word(&[e(ty, 0x1000, 0x2000)], 4 * GIB, 0x1010);
        match ty {
            0 | 10 => assert!(verdict.is_ok(), "type {ty}"),
            _ => assert_eq!(verdict, Err(NoLockWord::Type(Some(ty)))),
        }
    }

    // A dword off its boundary, in the firmware's own memory.
    for off in 1..4 {
        assert_eq!(lock_word(&Q35, 4 * GIB, Q35_FACS.0 + 16 + off), Err(NoLockWord::Misaligned));
    }
    // The last word the kernel maps, and the first it does not.
    let map = [e(10, 4 * GIB - 0x1000, 4 * GIB + 0x1000)];
    assert!(lock_word(&map, 4 * GIB, 4 * GIB - 4).is_ok());
    assert_eq!(lock_word(&map, 4 * GIB, 4 * GIB), Err(NoLockWord::Unmapped));
    assert_eq!(lock_word(&[e(10, u64::MAX - 0xFFF, u64::MAX)], 4 * GIB, u64::MAX - 3), Err(NoLockWord::Type(None)), "the address space's last dword");
}

/// What a crafted FADT names for `SMI_CMD`: `ACPI_ENABLE`, `ACPI_DISABLE`,
/// `S4BIOS_REQ` and `CST_CNT`, no `PSTATE_CNT`, and a reset register elsewhere.
const KEPT: KeptCommands = KeptCommands([Some(0xF0), Some(0xF1), Some(0xF2), None, Some(0x85), None]);

/// The kernel's declarations as q35 boots with them, with [`KEPT`] for
/// `SMI_CMD`'s, and the i8042's row.
fn standing(port: u16) -> Standing {
    match port {
        0x3F8..=0x3FF | 0x20..=0x21 | 0xA0..=0xA1 | 0x70..=0x71 | 0xCF8 | 0xCFC..=0xCFF | 0xCF9 => Standing::Declared(Mediated::Kept),
        0x80 => Standing::Declared(Mediated::Open),
        0xB2 => Standing::Declared(Mediated::Command(KEPT)),
        0x604..=0x605 | 0x660..=0x67F => Standing::Declared(Mediated::ReadOnly),
        0x60 | 0x64 => Standing::Row,
        _ => Standing::Free,
    }
}

const NO: PortVerdict = PortVerdict::Refused(Refused::KernelPort);

fn refused_port(refused: Refused) -> PortVerdict {
    PortVerdict::Refused(refused)
}

fn through(verdict: PortVerdict) -> Option<(u16, Width)> {
    match verdict {
        PortVerdict::Through(witness) => Some((witness.port(), witness.width())),
        _ => None,
    }
}

#[test]
fn a_port_answers_as_its_declaration_says() {
    for write in [None, Some(0)] {
        for (at, width) in [(0x3F8, Width::Byte), (0x3FF, Width::Byte), (0x20, Width::Word), (0x70, Width::Byte), (0xCF8, Width::DWord), (0xCF9, Width::Byte)] {
            assert_eq!(port(standing, at, width, write), NO, "{at:#x}");
        }
        assert_eq!(port(standing, 0x60, Width::Byte, write), refused_port(Refused::ClaimedPort));
        assert_eq!(port(standing, 0x64, Width::Byte, write), refused_port(Refused::ClaimedPort));
        // The POST port, and ports nothing declared.
        for (at, width) in [(0x80, Width::Byte), (0x72, Width::Word), (0x1800, Width::DWord), (0xFFFC, Width::DWord), (0xFFFF, Width::Byte)] {
            assert_eq!(through(port(standing, at, width, write)), Some((at, width)), "a free port");
        }
    }
    // The PM1a control block and the TCO block: read, never written.
    for (at, width) in [(0x604, Width::Word), (0x605, Width::Byte), (0x660, Width::DWord)] {
        assert!(through(port(standing, at, width, None)).is_some(), "{at:#x} reads");
        assert_eq!(port(standing, at, width, Some(0)), refused_port(Refused::ReadOnlyPort), "{at:#x}");
    }
}

/// The 8259 pair's edge/level control, which no declaration holds, is
/// refused both ways, an access at either end of it or across into it as
/// well; the ports beside it, the PIT, which the kernel neither declares nor
/// drives, and the PCI interrupt router's index and data, pass.
#[test]
fn the_8259s_edge_level_control_is_refused_though_nothing_declares_it() {
    for write in [None, Some(0)] {
        for (at, width) in [(0x4D0, Width::Byte), (0x4D1, Width::Byte), (0x4D0, Width::Word), (0x4CE, Width::DWord)] {
            assert_eq!(port(standing, at, width, write), refused_port(Refused::ProtectedPort), "{width:?} at {at:#x}");
        }
        for (at, width) in [
            (0x40, Width::DWord),
            (0x48, Width::DWord),
            (0x4CC, Width::DWord),
            (0x4D2, Width::Word),
            (0xC00, Width::Byte),
            (0xC01, Width::Byte),
        ] {
            assert_eq!(through(port(standing, at, width, write)), Some((at, width)), "{width:?} at {at:#x}");
        }
    }
}

/// `SMI_CMD` is read as a port is. A byte written to it is a call into the
/// firmware carrying that byte, for every byte but those the FADT gives a
/// meaning, zero among the callable where the tables name none with it, and
/// kept where they do, as a FACS whose `S4BIOS_F` is set over an
/// `S4BIOS_REQ` of zero does.
#[test]
fn a_byte_for_smi_cmd_is_a_firmware_call_unless_the_fadt_names_it() {
    assert_eq!(through(port(standing, 0xB2, Width::Byte, None)), Some((0xB2, Width::Byte)));
    for value in 0..=0xFFu64 {
        let verdict = port(standing, 0xB2, Width::Byte, Some(value));
        if [0xF0, 0xF1, 0xF2, 0x85].contains(&value) {
            assert_eq!(verdict, refused_port(Refused::KernelCommand), "{value:#04x}");
        } else {
            let PortVerdict::FirmwareCall(call) = verdict else { panic!("{value:#04x} answered {verdict:?}") };
            assert_eq!(u64::from(call.value()), value);
        }
    }
    // A FADT that names none keeps none.
    let unnamed = |_| Standing::Declared(Mediated::Command(KeptCommands([None; 6])));
    for value in [0u64, 0xF0, 0xFF] {
        assert!(matches!(port(unnamed, 0xB2, Width::Byte, Some(value)), PortVerdict::FirmwareCall(call) if u64::from(call.value()) == value));
    }
    // And one that names zero keeps zero.
    let zero = |_| Standing::Declared(Mediated::Command(KeptCommands([None, None, Some(0), None, None, None])));
    assert_eq!(port(zero, 0xB2, Width::Byte, Some(0)), refused_port(Refused::KernelCommand));
    assert!(matches!(port(zero, 0xB2, Width::Byte, Some(1)), PortVerdict::FirmwareCall(_)));
}

/// An AMD laptop's `SMI_CMD` is its reset register too, 0xb0 with
/// `RESET_VALUE` 0xfb beside `ACPI_ENABLE` 0xa0 and `ACPI_DISABLE` 0xa1: the
/// one declaration keeps the reset's byte with the FADT's own, so no write
/// asked of the port resets the machine, and every other byte is a call.
#[test]
fn a_smi_cmd_that_is_the_reset_register_keeps_the_resets_byte() {
    let shared = |port| match port {
        0xB0 => Standing::Declared(Mediated::Command(KeptCommands([Some(0xA0), Some(0xA1), None, None, None, Some(0xFB)]))),
        _ => Standing::Free,
    };
    for kept in [0xA0u64, 0xA1, 0xFB] {
        assert_eq!(port(shared, 0xB0, Width::Byte, Some(kept)), refused_port(Refused::KernelCommand), "{kept:#04x}");
    }
    for called in [0x00u64, 0x28, 0xFA, 0xFC, 0xFF] {
        assert!(matches!(port(shared, 0xB0, Width::Byte, Some(called)), PortVerdict::FirmwareCall(call) if u64::from(call.value()) == called), "{called:#04x}");
    }
    assert_eq!(through(port(shared, 0xB0, Width::Byte, None)), Some((0xB0, Width::Byte)));
}

/// A command is one byte to the one port: a wider write that reaches it,
/// from below or from it, is refused, whatever byte would land there, and so
/// is a value no byte holds.
#[test]
fn a_write_that_reaches_smi_cmd_and_is_no_byte_to_it_is_refused() {
    for (at, width) in [(0xB2, Width::Word), (0xB1, Width::Word), (0xB2, Width::DWord), (0xAF, Width::DWord)] {
        for value in [0u64, 0x10, 0xF1, 0xF100, 0x10_0000] {
            assert_eq!(port(standing, at, width, Some(value)), refused_port(Refused::CommandSpan), "{width:?} at {at:#x}");
        }
        assert_eq!(through(port(standing, at, width, None)), Some((at, width)), "{width:?} at {at:#x} reads");
    }
    assert_eq!(port(standing, 0xB2, Width::Byte, Some(0x100)), refused_port(Refused::CommandSpan));
    // Beside it, a port nothing declared.
    assert_eq!(through(port(standing, 0xB3, Width::Byte, Some(0xF1))), Some((0xB3, Width::Byte)));
    assert_eq!(through(port(standing, 0xAE, Width::DWord, Some(0xF1))), Some((0xAE, Width::DWord)));
}

/// [`CALLS`] are admitted however close together, and the next only once the
/// oldest of them is a whole period old: in no period, wherever it begins,
/// are there more.
#[test]
fn the_kernel_admits_a_fixed_few_firmware_calls_in_any_period() {
    let mut rate = CallRate::new();
    // A storm: one a millisecond, as firmware's AML retries an unanswered call.
    let admitted: Vec<u64> = (0..10_000u64).map(|ms| ms * 1_000_000).filter(|&now| rate.admit(now)).collect();
    assert_eq!(admitted.len(), 10 * CALLS, "ten seconds of a storm");
    for (i, &at) in admitted.iter().enumerate() {
        let within = admitted[i..].iter().take_while(|&&later| later - at < CALL_PERIOD_NS).count();
        assert!(within <= CALLS, "{within} calls in the period from {at}");
    }

    // The edge: all at one instant, then one nanosecond short of the period, then at it.
    let mut rate = CallRate::new();
    let from = 5_000_000_000;
    assert!((0..CALLS).all(|_| rate.admit(from)));
    assert!(!rate.admit(from));
    assert!(!rate.admit(from + CALL_PERIOD_NS - 1));
    assert!(rate.admit(from + CALL_PERIOD_NS));
    // That one replaced the first of the eight, so the next waits on the second, which is as old.
    assert!((1..CALLS).all(|_| rate.admit(from + CALL_PERIOD_NS)));
    assert!(!rate.admit(from + CALL_PERIOD_NS));
    assert!(!rate.admit(from + 2 * CALL_PERIOD_NS - 1));

    // A refused call is not kept: refusals do not push the next admission out.
    let mut rate = CallRate::new();
    assert!((0..CALLS).all(|_| rate.admit(0)));
    assert!((1..CALL_PERIOD_NS / 1_000_000).all(|ms| !rate.admit(ms * 1_000_000)));
    assert!(rate.admit(CALL_PERIOD_NS));

    // A clock that reads earlier than an admission admits nothing early.
    let mut rate = CallRate::new();
    assert!((0..CALLS).all(|_| rate.admit(CALL_PERIOD_NS)));
    assert!(!rate.admit(0));

    // Spaced a period's eighth apart, every call is admitted.
    let mut rate = CallRate::new();
    assert!((0..1000u64).all(|i| rate.admit(i * (CALL_PERIOD_NS / CALLS as u64))));
}

#[test]
fn a_wide_access_is_held_to_every_port_it_spans() {
    // A word that begins on a free port and ends on a kept one, and the same
    // into a row, a read-only run and out of the port space.
    assert_eq!(port(standing, 0x3F7, Width::Word, None), NO);
    assert_eq!(port(standing, 0x1D, Width::DWord, None), NO);
    assert_eq!(port(standing, 0x5F, Width::Word, Some(0)), refused_port(Refused::ClaimedPort));
    assert_eq!(port(standing, 0x603, Width::Word, Some(0)), refused_port(Refused::ReadOnlyPort));
    assert!(through(port(standing, 0x603, Width::Word, None)).is_some());
    assert_eq!(port(standing, 0xFFFF, Width::Word, None), refused_port(Refused::PortSpan));
    assert_eq!(port(standing, 0xFFFD, Width::DWord, Some(0)), refused_port(Refused::PortSpan));
    assert_eq!(port(standing, 0x1800, Width::QWord, None), refused_port(Refused::PortSpan), "no port access is a qword");
}

const HOST_BRIDGE: Function = Function { bus: 0, device: 0, function: 0 };

#[test]
fn a_configuration_read_is_held_to_the_window_and_to_one_register() {
    let ecam = ecam(0xe000_0000, 0, 0x7F);
    for (offset, width) in [(0, Width::DWord), (0xE, Width::Byte), (0x19, Width::Byte), (0x4A, Width::Word), (0xFFC, Width::DWord), (0xFFF, Width::Byte)] {
        let at = config(ecam, 0, HOST_BRIDGE, offset, width).expect("one register of a reachable function");
        assert_eq!((at.function(), at.offset(), at.width()), (HOST_BRIDGE, offset, width));
    }
    let last = Function { bus: 0x7F, device: 31, function: 7 };
    assert!(config(ecam, 0, last, 0, Width::DWord).is_ok());
    for (offset, width) in [(0, Width::QWord), (1, Width::DWord), (3, Width::Word), (0xFFE, Width::DWord), (0x1000, Width::Byte), (u16::MAX, Width::Byte)] {
        assert_eq!(config(ecam, 0, HOST_BRIDGE, offset, width), Err(Refused::ConfigSpan), "{offset:#x} {width:?}");
    }
    assert_eq!(config(ecam, 1, HOST_BRIDGE, 0, Width::DWord), Err(Refused::ConfigUnreachable), "another segment group");
    assert_eq!(config(ecam, 0, Function { bus: 0x80, device: 0, function: 0 }, 0, Width::DWord), Err(Refused::ConfigUnreachable));
    assert_eq!(config(ecam, 0, Function { bus: 0, device: 32, function: 0 }, 0, Width::DWord), Err(Refused::ConfigUnreachable));
    assert_eq!(config(ecam, 0, Function { bus: 0, device: 0, function: 8 }, 0, Width::DWord), Err(Refused::ConfigUnreachable));
    assert_eq!(config(&[], 0, HOST_BRIDGE, 0, Width::DWord), Err(Refused::ConfigUnreachable));
}

/// A function nothing claims or drives, of no class the kernel keeps.
const FREE: Enumerated = Enumerated { vendor: 0x1b36, device: 0x0010, class: 0x01, subclass: 0x08, held: false };

/// The T14's host bridge (`8086:9A14`, 631121-012 Table 15-3: UP3 4 cores),
/// and the AMD bench's root complex (`1022:1630`).
const T14_HOST_BRIDGE: Enumerated = Enumerated { vendor: 0x8086, device: 0x9A14, class: 0x06, subclass: 0x00, held: false };
const AMD_ROOT_COMPLEX: Enumerated = Enumerated { vendor: 0x1022, device: 0x1630, class: 0x06, subclass: 0x00, held: false };

/// A write of `width` at `offset` to a function the kernel enumerated as
/// `found`, or to one nothing answers at, decided whole: shape, then write.
fn config_write(found: Option<Enumerated>, offset: u16, width: Width) -> Result<u16, Refused> {
    let ecam = ecam(0xc000_0000, 0, 0x79);
    config(ecam, 0, HOST_BRIDGE, offset, width)?.write(found).map(|passed| passed.at().offset())
}

/// Every width of every register in `first..end`, each as one access.
fn every_access(first: u16, end: u16) -> impl Iterator<Item = (u16, Width)> {
    (first..end).flat_map(|offset| {
        [Width::Byte, Width::Word, Width::DWord].into_iter().filter(move |width| offset % 4 + width.bytes() as u16 <= 4).map(move |width| (offset, width))
    })
}

/// 00h–3Fh of every function is its header (PCI Express Base 6.0 §7.5.1),
/// which places its decode, its BARs and a bridge's buses and windows:
/// refused [`Refused::AddressMap`] in each width, on a function nothing
/// claims and on one nothing answers at, by its address and through ECAM.
/// From 40h such a function's write passes.
#[test]
fn a_configuration_write_is_refused_in_every_header() {
    for found in [Some(FREE), Some(AMD_ROOT_COMPLEX), None] {
        for (offset, width) in every_access(0, 0x40) {
            assert_eq!(config_write(found, offset, width), Err(Refused::AddressMap), "{found:?} {offset:#x} {width:?}");
        }
        assert_eq!(config_write(found, 0x40, Width::Byte), Ok(0x40), "{found:?}");
        assert_eq!(config_write(found, 0x100, Width::DWord), Ok(0x100), "{found:?}");
        assert_eq!(config_write(found, 0xFFC, Width::DWord), Ok(0xFFC), "{found:?}");
    }
    // Through ECAM it is the same access.
    let laptop = laptop();
    let MemoryVerdict::AsConfig(function, offset) = laptop.decide(0xc000_0000 + (0x1f << 15 | 0x3C), Width::Byte, true) else { panic!("in the window") };
    let ecam = ecam(0xc000_0000, 0, 0xFF);
    assert_eq!(config(ecam, 0, function, offset, Width::Byte).and_then(|at| at.write(Some(FREE)).map(drop)), Err(Refused::AddressMap));
}

/// A function a kernel driver drives or a claim holds is its driver's whole,
/// and an interrupt controller (base class 08h, subclass 00h) and an IOMMU
/// (08h, 06h) are the kernel's whether or not it programs them (PCI Code and
/// ID Assignment Specification): refused [`Refused::HeldFunction`] past the
/// header and in it. Another system peripheral is not.
#[test]
fn a_held_function_is_refused_whole() {
    let held = Enumerated { held: true, ..FREE };
    let interrupt_controller = Enumerated { vendor: 0x8086, device: 0x0001, class: 0x08, subclass: 0x00, held: false };
    let iommu = Enumerated { vendor: 0x1022, device: 0x1631, class: 0x08, subclass: 0x06, held: false };
    for found in [held, interrupt_controller, iommu] {
        for (offset, width) in [(0x04, Width::Word), (0x40, Width::Byte), (0xB8, Width::DWord), (0x100, Width::DWord), (0xFFF, Width::Byte)] {
            assert_eq!(config_write(Some(found), offset, width), Err(Refused::HeldFunction), "{found:?} {offset:#x}");
        }
    }
    for subclass in [0x01, 0x02, 0x03, 0x04, 0x05, 0x80] {
        let peripheral = Enumerated { class: 0x08, subclass, ..interrupt_controller };
        assert_eq!(config_write(Some(peripheral), 0x40, Width::Byte), Ok(0x40), "08h {subclass:#04x}");
    }
    // A read is a read: the policy holds its shape and nothing else.
    let ecam = ecam(0xc000_0000, 0, 0x79);
    assert!(config(ecam, 0, HOST_BRIDGE, 0x40, Width::Byte).is_ok());
}

/// On a host bridge 631122 covers, 40h–BFh is its address map (631122
/// §3.1.13–§3.1.34): PXPEPBAR 40h, MCHBAR 48h, GGC 50h, DEVEN 54h, PAVPC 58h,
/// DPR 5Ch, PCIEXBAR 60h, DMIBAR 68h, PAM0–6 and LAC 80h–87h, TOM A0h, TOUUD
/// A8h, BDSM B0h, BGSM B4h, TSEGMB B8h and TOLUD BCh. Refused there in each
/// width; from C0h, and on any other host bridge, it passes: the AMD bench's
/// root complex is written at B8h and BCh, where its `ALIB` mailbox is.
#[test]
fn the_host_bridges_address_map_is_refused_where_631122_maps_it() {
    for device in [0x9A02, 0x9A04, 0x9A12, 0x9A14, 0x9A1A] {
        let covered = Enumerated { device, ..T14_HOST_BRIDGE };
        for (offset, width) in every_access(0x40, 0xC0) {
            assert_eq!(config_write(Some(covered), offset, width), Err(Refused::AddressMap), "{device:#x} {offset:#x} {width:?}");
        }
        for (offset, width) in [(0xC0, Width::DWord), (0xC8, Width::Word), (0xDC, Width::DWord), (0x100, Width::DWord)] {
            assert_eq!(config_write(Some(covered), offset, width), Ok(offset), "{device:#x} {offset:#x}");
        }
    }
    // The H line, q35's MCH and the AMD bench's root complex.
    for other in [Enumerated { device: 0x9A26, ..T14_HOST_BRIDGE }, Enumerated { device: 0x9A36, ..T14_HOST_BRIDGE }, Enumerated { device: 0x29C0, ..T14_HOST_BRIDGE }, AMD_ROOT_COMPLEX] {
        for (offset, width) in [(0x48, Width::DWord), (0x60, Width::DWord), (0xB8, Width::DWord), (0xBC, Width::DWord), (0xBC, Width::Byte)] {
            assert_eq!(config_write(Some(other), offset, width), Ok(offset), "{other:?} {offset:#x}");
        }
    }
    // Another vendor's device of a covered number is no covered host bridge.
    assert_eq!(config_write(Some(Enumerated { vendor: 0x1022, ..T14_HOST_BRIDGE }), 0x48, Width::DWord), Ok(0x48));
}

/// The registers that place a port block the kernel declared: the 500 Series
/// eSPI controller's ESPI_IOD_IOE (631119-007 Table 6, A080h–A09Fh;
/// 631120-002 §2.1.6), which enables COM1's and the i8042's decode, and where
/// each chipset the watchdog knows places and enables its TCO block
/// (`toyos_tco::CHIPSETS`): Tiger Lake-LP's SMBus function at TCOBASE 50h and
/// TCOCTL 54h (631120-002 §6.1.17, §6.1.18), and q35's LPC bridge at 40h.
/// Refused [`Refused::AddressMap`]; the registers beside them pass.
#[test]
fn a_register_that_places_a_declared_port_block_is_refused() {
    let espi = |device| Enumerated { vendor: 0x8086, device, class: 0x06, subclass: 0x01, held: false };
    for device in [0xA080, 0xA082, 0xA087, 0xA09F] {
        for (offset, width) in every_access(0x80, 0x84) {
            assert_eq!(config_write(Some(espi(device)), offset, width), Err(Refused::AddressMap), "{device:#x} {offset:#x} {width:?}");
        }
        assert_eq!(config_write(Some(espi(device)), 0x84, Width::DWord), Ok(0x84), "LGIR1");
        assert_eq!(config_write(Some(espi(device)), 0x7C, Width::DWord), Ok(0x7C));
    }
    for device in [0xA07F, 0xA0A0] {
        assert_eq!(config_write(Some(espi(device)), 0x80, Width::DWord), Ok(0x80), "{device:#x}");
    }
    let smbus = Enumerated { vendor: 0x8086, device: 0xA0A3, class: 0x0C, subclass: 0x05, held: false };
    for (offset, width) in every_access(0x50, 0x58) {
        assert_eq!(config_write(Some(smbus), offset, width), Err(Refused::AddressMap), "{offset:#x} {width:?}");
    }
    assert_eq!(config_write(Some(smbus), 0x40, Width::DWord), Ok(0x40), "HOSTC");
    assert_eq!(config_write(Some(smbus), 0x58, Width::DWord), Ok(0x58));
    let q35_lpc = Enumerated { vendor: 0x8086, device: 0x2918, class: 0x06, subclass: 0x01, held: false };
    for (offset, width) in every_access(0x40, 0x44) {
        assert_eq!(config_write(Some(q35_lpc), offset, width), Err(Refused::AddressMap), "{offset:#x} {width:?}");
    }
    assert_eq!(config_write(Some(q35_lpc), 0x44, Width::DWord), Ok(0x44));
}

/// MCHBAR's window as 00:00.0 of the T14 places it, 128 KiB aligned.
const T14_MCHBAR: u64 = 0xFEDC_0000;

/// The T14's shape: RAM low, the ECAM window at 0xC0000000 for buses
/// 0..=0x79 (`pr-875` `round3/t14-testcases.log` line 160), everything from it
/// to 4 GiB typed uncacheable and listed nowhere, its four VT-d units driven,
/// and its host bridge with MCHBAR on.
fn t14() -> Mem {
    const MAP: &[MemoryMapEntry] = &[e(7, 0, 0x8000_0000)];
    const DRIVEN: &[(u64, u64)] = &[(0xFED9_0000, 0xFED9_1000), (0xFED8_5000, 0xFED8_6000), (0xFED8_6000, 0xFED8_7000), (0xFED9_1000, 0xFED9_2000)];
    Memory {
        map: MAP,
        mapped_end: 4 * GIB,
        ecam: ecam(0xc000_0000, 0, 0x79),
        devices: devices(DRIVEN),
        facs: None,
        uncached: |at, len| at >= 0xc000_0000 && at + len <= 4 * GIB,
        registers_differ: false,
        host_bridge: HostBridge { vendor: 0x8086, device: 0x9A14, at_48h: T14_MCHBAR | 1 },
    }
}

/// On a host bridge 631122 covers whose MCHBAR is on, a write that overlaps
/// a register the kernel keeps there is refused
/// [`Refused::KernelRegister`], in every width, wherever firmware's map types
/// its page: GFXVTBAR 5400h–5407h and VTDPVC0BAR 5410h–5417h (§3.4.2,
/// §3.4.4), INTRDIRCTL 5418h–541Bh (§3.4.5) and PACKAGE_RAPL_LIMIT
/// 59A0h–59A7h (§3.3.31). Every read passes, as do the T14's writes to the
/// cTDP control at 5F50h and the turbo activation ratio at 5F54h, and the
/// bytes either side of each register.
#[test]
fn a_register_the_kernel_keeps_is_refused_kernelregister() {
    const KEPT: [(u64, u64); 4] = [(0x5400, 0x5408), (0x5410, 0x5418), (0x5418, 0x541C), (0x59A0, 0x59A8)];
    // The same machine with MCHBAR's page typed reserved.
    const RESERVED: &[MemoryMapEntry] = &[e(7, 0, 0x8000_0000), e(0, T14_MCHBAR, T14_MCHBAR + 0x2_0000)];
    for memory in [t14(), Memory { map: RESERVED, ..t14() }] {
        for (start, end) in KEPT {
            for width in [Width::Byte, Width::Word, Width::DWord, Width::QWord] {
                // Every access that holds a byte of the register.
                for at in (T14_MCHBAR + start + 1).saturating_sub(width.bytes())..T14_MCHBAR + end {
                    assert_eq!(refused(&memory, at, width, true), Refused::KernelRegister, "{at:#x} {width:?}");
                    assert!(passes(&memory, at, width, false), "{at:#x} {width:?} reads");
                }
            }
        }
        for (at, width) in [(0x59A0 - 1, Width::Byte), (0x59A8, Width::Byte), (0x5408, Width::QWord), (0x541C, Width::DWord), (0x53F8, Width::QWord), (0x5F50, Width::Byte), (0x5F54, Width::Byte), (0x5F50, Width::DWord)] {
            assert!(passes(&memory, T14_MCHBAR + at, width, true), "{at:#x} {width:?}");
        }
    }
}

/// The kept registers are where 631122 has them only on the host bridges it
/// covers (631121-012 Table 15-3: 9A02h, 9A04h, 9A12h, 9A14h and 9A1Ah), and
/// only where MCHBAREN, bit 0 of 48h, is set (§3.1.14): on the H line, q35's
/// MCH, the AMD bench's root complex, a disabled window or nothing at
/// 00:00.0 the same write passes.
#[test]
fn kept_registers_exist_only_on_the_host_bridges_631122_covers() {
    let rapl = T14_MCHBAR + 0x59A0;
    for device in [0x9A02, 0x9A04, 0x9A12, 0x9A14, 0x9A1A] {
        let memory = Memory { host_bridge: HostBridge { vendor: 0x8086, device, at_48h: T14_MCHBAR | 1 }, ..t14() };
        assert_eq!(refused(&memory, rapl, Width::DWord, true), Refused::KernelRegister, "{device:#x}");
    }
    for host_bridge in [
        HostBridge { vendor: 0x8086, device: 0x9A26, at_48h: T14_MCHBAR | 1 },
        HostBridge { vendor: 0x8086, device: 0x9A36, at_48h: T14_MCHBAR | 1 },
        HostBridge { vendor: 0x8086, device: 0x29C0, at_48h: T14_MCHBAR | 1 },
        HostBridge { vendor: 0x1022, device: 0x1630, at_48h: T14_MCHBAR | 1 },
        HostBridge { vendor: 0x1022, device: 0x9A14, at_48h: T14_MCHBAR | 1 },
        HostBridge { vendor: 0x8086, device: 0x9A14, at_48h: T14_MCHBAR },
        HostBridge::ABSENT,
    ] {
        assert!(passes(&Memory { host_bridge, ..t14() }, rapl, Width::DWord, true), "{host_bridge:?}");
    }
    // The window's base is bits 38:17: the low bits beside the enable, and
    // anything past bit 38, place nothing.
    let noisy = Memory { host_bridge: HostBridge { vendor: 0x8086, device: 0x9A14, at_48h: 1 << 45 | T14_MCHBAR | 0x1_FFFF }, ..t14() };
    assert_eq!(refused(&noisy, rapl, Width::Byte, true), Refused::KernelRegister);
}

/// `SLP_TYPx` is bits 12:10 of PM1 control and `SLP_EN` bit 13 (ACPI 6.5
/// Table 4.16): each of the eight types lands in the field and nowhere else,
/// whatever the register held, and a word one wider, which shifted would set
/// `SLP_EN`, names none.
#[test]
fn a_sleep_type_is_three_bits_and_lands_only_in_its_field() {
    const SLP_EN: u16 = 1 << 13;
    for word in 0..=7u64 {
        let slp_typ = sleep_type(word).unwrap_or_else(|| panic!("{word} is a sleep type"));
        assert_eq!(u64::from(slp_typ.get()), word);
        for held in [0u16, 0x0001, 0x1C00, 0xFFFF, SLP_EN | 0x0401] {
            let typed = slp_typ.in_control(held);
            assert_eq!(u64::from(typed >> 10 & 7), word, "{word} over {held:#06x}");
            assert_eq!(typed & !0x1C00, held & !0x1C00, "{word} over {held:#06x} moved a bit outside SLP_TYPx");
        }
    }
    for wide in [8, 9, 0xFF, 0x100, 1 << 10, 7 << 10, 1 << 32 | 5, u64::MAX] {
        assert_eq!(sleep_type(wide), None, "{wide:#x}");
    }
}
