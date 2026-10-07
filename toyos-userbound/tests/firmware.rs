//! The `acpi` claim's mediated access, row by row: what passes, what each
//! refusal is called, and that the edges of every range fall on the right
//! side.

use toyos_abi::acpi::{Refused, Width};
use toyos_abi::boot::MemoryMapEntry;
use toyos_userbound::firmware::{config, config_write, port, Ecam, Function, Memory, MemoryVerdict, Standing};
use toyos_userbound::Mediated;

const fn e(uefi_type: u32, start: u64, end: u64) -> MemoryMapEntry {
    MemoryMapEntry { uefi_type, start, end }
}

const GIB: u64 = 1 << 30;

/// Descriptors of the map QEMU 11.1.1's edk2 hands a 2 GiB q35 guest
/// (`toyos-bootmap/tests/direct_map.rs` holds it whole), with an ACPI reclaim
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
const Q35_ECAM: Ecam = Ecam { base: 0xe000_0000, segment: 0, first_bus: 0, last_bus: 0xFF };
/// The I/O APIC and the HPET, as the kernel maps them on q35.
const Q35_DRIVEN: [(u64, u64); 2] = [(0xfec0_0000, 0xfec0_0020), (0xfed0_0000, 0xfed0_1000)];
const Q35_FACS: (u64, u64) = (0x7ff7_7000, 0x7ff7_7040);

fn q35() -> Memory<'static> {
    Memory { map: &Q35, mapped_end: 4 * GIB, ecam: Some(Q35_ECAM), driven: &Q35_DRIVEN, facs: Some(Q35_FACS) }
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

fn laptop() -> Memory<'static> {
    let ecam = Ecam { base: 0xc000_0000, segment: 0, first_bus: 0, last_bus: 0xFF };
    Memory { map: &LAPTOP, mapped_end: 4 * GIB, ecam: Some(ecam), driven: &[], facs: Some((0x7400_0040, 0x7400_0080)) }
}

fn passes(memory: &Memory, at: u64, width: Width, write: bool) -> bool {
    match memory.decide(at, width, write) {
        MemoryVerdict::Through(witness) => {
            assert_eq!((witness.at(), witness.width()), (at, width), "the witness names another access");
            true
        }
        _ => false,
    }
}

fn refused(memory: &Memory, at: u64, width: Width, write: bool) -> Refused {
    match memory.decide(at, width, write) {
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
    assert_eq!(q35.type_word(0x100000), 7);
    assert_eq!(q35.type_word(0x87000), 4);
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
    assert_eq!(q35.type_word(0x800000), 10);
    assert_eq!(laptop.type_word(0x7000_0000), 0);
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

#[test]
fn every_other_type_and_an_unlisted_address_is_refused_with_its_type() {
    let laptop = laptop();
    for write in [false, true] {
        // Runtime services data, and a hole the map does not list.
        assert_eq!(refused(&laptop, 0x7490_1000, Width::Byte, write), Refused::MemoryType);
        assert_eq!(refused(&laptop, 0x8000_0000, Width::Byte, write), Refused::MemoryType);
    }
    assert_eq!(laptop.type_word(0x7490_1000), 6);
    assert_eq!(laptop.type_word(0x8000_0000), toyos_abi::acpi::UNLISTED);
    // Every type but the three the policy names, as the only range of a map.
    for ty in (0..=0x20u32).chain([0x7000_0000, 0x8000_0000, u32::MAX]) {
        let map = [e(ty, 0x1000, 0x2000)];
        let memory = Memory { map: &map, mapped_end: 4 * GIB, ecam: None, driven: &[], facs: None };
        let read = memory.decide(0x1000, Width::Byte, false);
        let write = memory.decide(0x1000, Width::Byte, true);
        let through = |verdict| matches!(verdict, MemoryVerdict::Through(_));
        match ty {
            0 | 10 => assert!(through(read) && through(write), "type {ty}"),
            9 => assert!(through(read) && write == MemoryVerdict::Refused(Refused::TableWrite)),
            1 | 2 | 3 | 4 | 7 => assert!(read == write && read == MemoryVerdict::Refused(Refused::UsableMemory), "type {ty}"),
            _ => assert!(read == write && read == MemoryVerdict::Refused(Refused::MemoryType), "type {ty}"),
        }
    }
}

#[test]
fn memory_past_what_the_kernel_maps_is_refused() {
    let q35 = q35();
    assert_eq!(refused(&q35, 0xfd_0000_0000, Width::Byte, false), Refused::Unmapped);
    let map = [e(0, 4 * GIB - 0x1000, 4 * GIB + 0x1000)];
    let memory = Memory { map: &map, mapped_end: 4 * GIB, ecam: None, driven: &[], facs: None };
    assert!(passes(&memory, 4 * GIB - 8, Width::QWord, true));
    assert_eq!(refused(&memory, 4 * GIB - 7, Width::QWord, false), Refused::Unmapped);
    assert_eq!(refused(&memory, 4 * GIB, Width::Byte, false), Refused::Unmapped);
    // An access that would wrap the address space names no memory.
    assert_eq!(refused(&q35, u64::MAX, Width::Word, false), Refused::Unmapped);
    assert_eq!(refused(&q35, u64::MAX - 6, Width::QWord, true), Refused::Unmapped);
}

#[test]
fn a_window_the_kernel_drives_a_device_through_is_refused_inside_any_type() {
    // The I/O APIC, the HPET and the local APIC, typed reserved by this map.
    let map = [e(0, 0xfec0_0000, 0xff00_0000)];
    let memory = Memory { map: &map, mapped_end: 4 * GIB, ecam: None, driven: &Q35_DRIVEN, facs: None };
    for write in [false, true] {
        assert_eq!(refused(&memory, 0xfec0_0000, Width::DWord, write), Refused::KernelDevice);
        assert_eq!(refused(&memory, 0xfec0_001f, Width::Byte, write), Refused::KernelDevice);
        assert_eq!(refused(&memory, 0xfed0_0ff8, Width::QWord, write), Refused::KernelDevice);
        // An access that begins before a window and ends in it.
        assert_eq!(refused(&memory, 0xfed0_0000 - 1, Width::Word, write), Refused::KernelDevice);
        assert_eq!(refused(&memory, 0xfee0_0000, Width::DWord, write), Refused::KernelDevice);
        assert_eq!(refused(&memory, 0xfeef_ffff, Width::Byte, write), Refused::KernelDevice);
        assert_eq!(refused(&memory, 0xfee0_0000 - 4, Width::QWord, write), Refused::KernelDevice);
        // The bytes either side of each window are the firmware's.
        assert!(passes(&memory, 0xfec0_0020, Width::DWord, write));
        assert!(passes(&memory, 0xfed0_1000, Width::Byte, write));
        assert!(passes(&memory, 0xfee0_0000 - 8, Width::QWord, write));
        assert!(passes(&memory, 0xfef0_0000, Width::Byte, write));
    }
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
        let base = memory.ecam.expect("an ECAM window").base;
        for write in [false, true] {
            assert_eq!(
                memory.decide(base + (3 << 20 | 0x1c << 15 | 5 << 12 | 0x48), Width::DWord, write),
                MemoryVerdict::AsConfig(Function { bus: 3, device: 0x1c, function: 5 }, 0x48)
            );
            assert_eq!(
                memory.decide(base + (0xFF << 20 | 0x1f << 15 | 7 << 12 | 0xFFF), Width::Byte, write),
                MemoryVerdict::AsConfig(Function { bus: 0xFF, device: 0x1f, function: 7 }, 0xFFF),
                "the window's last byte"
            );
            assert_eq!(refused(&memory, base - 1, Width::Word, write), Refused::Straddles);
            assert_eq!(refused(&memory, base + (0x100 << 20) - 1, Width::Word, write), Refused::Straddles);
        }
    }
    // A window that begins at a later bus holds nothing below it.
    let map = [e(0, 0xe000_0000, 0xf000_0000)];
    let ecam = Ecam { base: 0xe000_0000, segment: 0, first_bus: 0x10, last_bus: 0x1F };
    let memory = Memory { map: &map, mapped_end: 4 * GIB, ecam: Some(ecam), driven: &[], facs: None };
    assert!(passes(&memory, 0xe000_0000, Width::Byte, false), "below the first bus is plain reserved memory");
    assert_eq!(memory.decide(0xe100_0000, Width::Byte, false), MemoryVerdict::AsConfig(Function { bus: 0x10, device: 0, function: 0 }, 0));
    assert!(passes(&memory, 0xe200_0000, Width::Byte, false), "past the last bus too");
    // A base firmware put at the top of the address space decides without overflow.
    let ecam = Ecam { base: u64::MAX - 0xFFF, segment: 0, first_bus: 0, last_bus: 0xFF };
    let memory = Memory { map: &map, mapped_end: 4 * GIB, ecam: Some(ecam), driven: &[], facs: None };
    assert!(passes(&memory, 0xe000_0000, Width::Byte, false));
}

/// The kernel's declarations as q35 boots with them, and the i8042's row.
fn standing(port: u16) -> Standing {
    match port {
        0x3F8..=0x3FF | 0x20..=0x21 | 0xA0..=0xA1 | 0x70..=0x71 | 0xCF8 | 0xCFC..=0xCFF | 0xCF9 => Standing::Declared(Mediated::Kept),
        0x80 => Standing::Declared(Mediated::Open),
        0xB2 | 0x604..=0x605 | 0x660..=0x67F => Standing::Declared(Mediated::ReadOnly),
        0x60 | 0x64 => Standing::Row,
        _ => Standing::Free,
    }
}

#[test]
fn a_port_answers_as_its_declaration_says() {
    for write in [false, true] {
        for (at, width) in [(0x3F8, Width::Byte), (0x3FF, Width::Byte), (0x20, Width::Word), (0x70, Width::Byte), (0xCF8, Width::DWord), (0xCF9, Width::Byte)] {
            assert_eq!(port(standing, at, width, write), Err(Refused::KernelPort), "{at:#x}");
        }
        assert_eq!(port(standing, 0x60, Width::Byte, write), Err(Refused::ClaimedPort));
        assert_eq!(port(standing, 0x64, Width::Byte, write), Err(Refused::ClaimedPort));
        // The POST port, and ports nothing declared.
        for (at, width) in [(0x80, Width::Byte), (0x72, Width::Word), (0x1800, Width::DWord), (0xFFFC, Width::DWord), (0xFFFF, Width::Byte)] {
            let witness = port(standing, at, width, write).expect("a free port");
            assert_eq!((witness.port(), witness.width()), (at, width));
        }
    }
    // SMI_CMD, the PM1a control block and the TCO block: read, never written.
    for (at, width) in [(0xB2, Width::Byte), (0x604, Width::Word), (0x605, Width::Byte), (0x660, Width::DWord)] {
        assert!(port(standing, at, width, false).is_ok(), "{at:#x} reads");
        assert_eq!(port(standing, at, width, true), Err(Refused::ReadOnlyPort), "{at:#x}");
    }
}

#[test]
fn a_wide_access_is_held_to_every_port_it_spans() {
    // A word that begins on a free port and ends on a kept one, and the same
    // into a row, a read-only run and out of the port space.
    assert_eq!(port(standing, 0x3F7, Width::Word, false), Err(Refused::KernelPort));
    assert_eq!(port(standing, 0x1D, Width::DWord, false), Err(Refused::KernelPort));
    assert_eq!(port(standing, 0x5F, Width::Word, true), Err(Refused::ClaimedPort));
    assert_eq!(port(standing, 0xB1, Width::Word, true), Err(Refused::ReadOnlyPort));
    assert!(port(standing, 0xB1, Width::Word, false).is_ok());
    assert_eq!(port(standing, 0xFFFF, Width::Word, false), Err(Refused::PortSpan));
    assert_eq!(port(standing, 0xFFFD, Width::DWord, true), Err(Refused::PortSpan));
    assert_eq!(port(standing, 0x1800, Width::QWord, false), Err(Refused::PortSpan), "no port access is a qword");
}

const HOST_BRIDGE: Function = Function { bus: 0, device: 0, function: 0 };

#[test]
fn a_configuration_read_is_held_to_the_window_and_to_one_register() {
    let ecam = Some(Ecam { base: 0xe000_0000, segment: 0, first_bus: 0, last_bus: 0x7F });
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
    assert_eq!(config(None, 0, HOST_BRIDGE, 0, Width::DWord), Err(Refused::ConfigUnreachable));
}

#[test]
fn a_configuration_write_lands_only_where_the_kernel_decides_nothing() {
    let ecam = Some(Ecam { base: 0xe000_0000, segment: 0, first_bus: 0, last_bus: 0xFF });
    let at = |offset, width| config(ecam, 0, HOST_BRIDGE, offset, width).expect("a readable register");
    // A power management capability at 0x50 and an MSI capability at 0x60.
    let programmed = [(0x50u8, 8u8), (0x60, 24)];
    let write = |offset, width, free| config_write(at(offset, width), free, programmed).map(|passed| passed.at());

    for (offset, width) in [(0x00, Width::DWord), (0x04, Width::Word), (0x10, Width::DWord), (0x3C, Width::Byte), (0x3F, Width::Byte)] {
        assert_eq!(write(offset, width, true), Err(Refused::ConfigHeader), "{offset:#x}");
    }
    for (offset, width) in [(0x100, Width::DWord), (0x100, Width::Byte), (0xFFC, Width::DWord)] {
        assert_eq!(write(offset, width, true), Err(Refused::ConfigExtended), "{offset:#x}");
    }
    for (offset, width) in [(0x50, Width::DWord), (0x54, Width::Word), (0x57, Width::Byte), (0x60, Width::Byte), (0x74, Width::DWord), (0x77, Width::Byte)] {
        assert_eq!(write(offset, width, true), Err(Refused::ConfigCapability), "{offset:#x}");
    }
    // The registers either side of each capability, and the last conventional one.
    for (offset, width) in [(0x40, Width::DWord), (0x4C, Width::DWord), (0x4F, Width::Byte), (0x58, Width::DWord), (0x5F, Width::Byte), (0x78, Width::Byte), (0xFC, Width::DWord), (0xFF, Width::Byte)] {
        assert_eq!(write(offset, width, true), Ok(at(offset, width)), "{offset:#x}");
        assert_eq!(write(offset, width, false), Err(Refused::ConfigDriven), "{offset:#x} on a function a driver holds");
    }
    // A capability that runs to the end of conventional space, as firmware's pointer may claim.
    assert_eq!(config_write(at(0xFF, Width::Byte), true, [(0xFCu8, 0xFFu8)]), Err(Refused::ConfigCapability));
}
