//! The pinned files against what the specification's container read-outs
//! state of them (§6, §7.4, Appendices A and B), every message against the
//! sizes it asserts (§7.5, §10, §11.1, §12), and the refusals.

use alloc::vec;
use alloc::vec::Vec;

use crate::command::{self, Band, Command, CoexistenceMode, Probe, ScanChannel, ScanRefused, ScanRequest, Segment};
use crate::container::{self, Firmware, Image, Refused};
use crate::notification::{self, Alive, CountryAnswer, IterationDone, Malformed, NvmInfo, Received};
use crate::part::{Hardware, NotServed, Part};
use crate::pnvm::{self, Choice, Pnvm};
use crate::versions::{self, Declared, Id, ReceivedForm, ScanForm, Side, Unsupported, Versions};

const AX200: &[u8] = include_bytes!("../../firmware/iwlwifi-cc-a0-77.ucode");
const AX210: &[u8] = include_bytes!("../../firmware/iwlwifi-ty-a0-gf-a0-89.ucode");
const AX210_PNVM: &[u8] = include_bytes!("../../firmware/iwlwifi-ty-a0-gf-a0.pnvm");

fn ax200() -> Firmware<'static> {
    Firmware::parse(AX200).expect("the AX200's pinned file decodes")
}

fn ax210() -> Firmware<'static> {
    Firmware::parse(AX210).expect("the AX210's pinned file decodes")
}

/// Sections, and their bytes without load addresses.
fn shape(sections: &[container::Section<'_>]) -> (usize, usize) {
    (sections.len(), sections.iter().map(|s| s.bytes.len()).sum())
}

#[test]
fn the_ax200s_file_decodes_as_its_read_out_states() {
    let f = ax200();
    assert_eq!(f.name, b"release/core74::563a6e92");
    assert_eq!((f.api_version, f.build), (77, 1_446_669_970));
    assert_eq!(f.version, container::Version { major: 77, minor: 0x563a_6e92, local: 0 });
    assert_eq!(f.hardware, Hardware { mac: 0x34, rf: 0x10a });
    assert_eq!((f.flags, f.phy_config), (0xfbfb_4b7f, 0x0033_0018));
    assert_eq!((f.probe_max, f.scan_channels, f.stations), (512, 67, 15));
    assert_eq!(shape(&f.lmac), (14, 373_648));
    assert_eq!(shape(&f.umac), (15, 389_308));
    assert_eq!(shape(&f.paging), (19, 587_384));
    assert_eq!(f.loader, None);
    assert_eq!((f.lmac_error_table, f.umac_error_table), (0x004d_ff20, 0x0088_0128));
    assert_eq!(f.declared.entries().count(), 192);
}

#[test]
fn the_ax210s_file_decodes_as_its_read_out_states() {
    let f = ax210();
    assert_eq!(f.name, b"release/core86:signed_remote:b20dcd1f");
    assert_eq!((f.api_version, f.build), (89, 2_987_248_927));
    assert_eq!(f.version, container::Version { major: 89, minor: 0xb20d_cd1f, local: 0 });
    assert_eq!(f.hardware, Hardware { mac: 0x42, rf: 0x10d });
    assert_eq!((f.flags, f.phy_config), (0xdbfb_4b7f, 0x0033_0018));
    assert_eq!((f.probe_max, f.scan_channels, f.stations), (512, 67, 15));
    assert_eq!(shape(&f.lmac), (15, 418_496));
    assert_eq!(shape(&f.umac), (16, 445_224));
    assert_eq!(shape(&f.paging), (25, 763_512));
    assert_eq!(f.loader.map(<[u8]>::len), Some(13_944));
    assert_eq!((f.lmac_error_table, f.umac_error_table), (0x004f_2358, 0x0047_1488));
    assert_eq!(f.declared.entries().count(), 218);
}

#[test]
fn each_image_starts_with_its_1656_byte_section_and_none_is_over_32_kib() {
    for f in [ax200(), ax210()] {
        for image in [Image::Lmac, Image::Umac, Image::Paging] {
            let sections = f.image(image);
            assert_eq!(sections.first().map(|s| s.bytes.len()), Some(1_656), "{image:?}");
            assert_eq!(sections.iter().map(|s| s.bytes.len()).max(), Some(32_768), "{image:?}");
        }
    }
}

const API_BOTH: &[u32] = &[
    0, 1, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 19, 20, 23, 24, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35,
    36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65,
];
const CAPABILITIES_BOTH: &[u32] = &[
    1, 2, 3, 4, 5, 6, 8, 9, 16, 17, 18, 19, 21, 22, 23, 24, 26, 27, 28, 31, 33, 34, 35, 37, 38, 39, 41, 43, 44, 45, 46,
    47, 48, 49, 50, 51, 52, 53, 54, 56, 57, 58, 59, 60, 61, 63, 64, 65, 67, 68, 70, 71, 73, 74, 75, 77, 78, 79, 80, 84,
    87, 88, 89, 91, 92, 94, 95, 98, 99, 101, 103, 105, 108, 110, 117,
];

fn with(both: &[u32], only: &[u32]) -> Vec<u32> {
    let mut bits: Vec<u32> = both.iter().chain(only).copied().collect();
    bits.sort_unstable();
    bits
}

#[test]
fn each_file_sets_the_api_and_capability_bits_its_read_out_lists() {
    let (a, b) = (ax200(), ax210());
    assert_eq!(a.api.set().collect::<Vec<_>>(), API_BOTH);
    assert_eq!(b.api.set().collect::<Vec<_>>(), with(API_BOTH, &[66, 67, 68]));
    assert_eq!(a.capabilities.set().collect::<Vec<_>>(), with(CAPABILITIES_BOTH, &[0, 97]));
    let ax210_only = [32, 96, 100, 104, 107, 111, 112, 113, 114, 116, 120, 123, 124, 129];
    assert_eq!(b.capabilities.set().collect::<Vec<_>>(), with(CAPABILITIES_BOTH, &ax210_only));
}

#[test]
fn every_message_the_driver_exchanges_is_declared_at_a_version_typed_here() {
    assert_eq!(Versions::of(&ax200().declared), Ok(Versions { scan: ScanForm::BandByte, received: ReceivedForm::V4 }));
    assert_eq!(Versions::of(&ax210().declared), Ok(Versions { scan: ScanForm::BandInFlags, received: ReceivedForm::V5 }));
}

/// §10.1's declared versions, as each file's command-version record has them.
#[test]
fn the_files_declare_what_the_summary_table_says() {
    let (a, b) = (ax200().declared, ax210().declared);
    let rows: &[(versions::Message, Option<u8>, Option<u8>)] = &[
        (versions::ALIVE, Some(6), Some(6)),
        (versions::INIT_FLAGS, None, None),
        (versions::NVM_DONE, Some(1), Some(1)),
        (versions::NVM_INFO, Some(1), Some(1)),
        (versions::NVM_INFO_ANSWER, Some(4), Some(4)),
        (versions::TRANSMIT_ANTENNAS, Some(1), Some(1)),
        (versions::COEXISTENCE, Some(6), Some(6)),
        (versions::PLATFORM_LATENCY, Some(2), Some(2)),
        (versions::LATENCY_TOLERANCE, Some(3), Some(3)),
        (versions::TEMPERATURE_THRESHOLDS, Some(1), Some(1)),
        (versions::DEVICE_POWER, Some(6), Some(7)),
        (versions::COUNTRY_UPDATE, Some(1), Some(1)),
        (versions::COUNTRY_ANSWER, Some(5), Some(6)),
        (versions::SCAN_CONFIGURATION, Some(5), Some(5)),
        (versions::BEACON_FILTER, Some(4), Some(4)),
        (versions::SCAN_REQUEST, Some(15), Some(17)),
        (versions::SCAN_ABORT, Some(1), Some(1)),
        (versions::PNVM_LOADED, Some(1), Some(1)),
        (versions::RECEIVED_FRAME, Some(4), Some(5)),
        (versions::THERMAL_SHUTDOWN, Some(2), Some(2)),
    ];
    for &(message, ax200, ax210) in rows {
        assert_eq!(a.of(message.id, message.side), ax200, "{}", message.name);
        assert_eq!(b.of(message.id, message.side), ax210, "{}", message.name);
    }
}

#[test]
fn a_message_declared_at_a_version_without_a_layout_refuses_the_file() {
    let entry = |id: u8, group: u8, command: u8, notification: u8| [id, group, command, notification];
    let mut record: Vec<u8> = ax200().declared.entries().flat_map(|(id, e)| entry(id.id, id.group, e.command, e.notification)).collect();
    // The scan request (0x01, 0x0d) at 16, a version nothing here is written for.
    let at = record.chunks(4).position(|e| e[0] == 0x0d && e[1] == 0x01).expect("the scan request's entry") * 4;
    record[at + 2] = 16;
    let declared = Declared::parse(&record).expect("whole entries");
    assert_eq!(Versions::of(&declared), Err(Unsupported::Version { message: "scan request", declared: Some(16) }));
    // An unknown version is the caller's default, as no entry is: no layout's, for a message
    // Intel gives several.
    record[at + 2] = versions::UNKNOWN;
    let declared = Declared::parse(&record).expect("whole entries");
    assert_eq!(Versions::of(&declared), Err(Unsupported::Version { message: "scan request", declared: None }));
    // Nor is no entry, for a message Intel gives several layouts.
    record.drain(at..at + 4);
    let declared = Declared::parse(&record).expect("whole entries");
    assert_eq!(Versions::of(&declared), Err(Unsupported::Version { message: "scan request", declared: None }));
}

/// The first entry answers a lookup (§6.5): the pinned files' (0x0e, 0x17) reads command 1 and
/// notification 0, and a second entry after a message's own changes nothing.
#[test]
fn a_lookup_takes_the_first_entry_for_its_message() {
    let twice = Id { group: 0x0e, id: 0x17 };
    for f in [ax200(), ax210()] {
        assert_eq!((f.declared.of(twice, Side::Command), f.declared.of(twice, Side::Notification)), (Some(1), Some(0)));
    }
    let mut record: Vec<u8> = ax200().declared.entries().flat_map(|(id, e)| [id.id, id.group, e.command, e.notification]).collect();
    record.extend([0x0d, 0x01, 16, 0]);
    let declared = Declared::parse(&record).expect("whole entries");
    assert_eq!(Versions::of(&declared), Ok(Versions { scan: ScanForm::BandByte, received: ReceivedForm::V4 }));
    assert_eq!(Declared::parse(&record[..7]), None);
}

#[test]
fn the_platform_nvm_decodes_as_its_read_out_states() {
    let pnvm = Pnvm::parse(AX210_PNVM).expect("the pinned platform NVM decodes");
    let skus = [0x0006_10d0, 0x0005_10d0, 0x0045_10d0, 0x0046_10d0];
    let second = [11_996, 12_048, 12_048, 12_000];
    assert_eq!(pnvm.blocks.len(), 4);
    for ((block, sku), second) in pnvm.blocks.iter().zip(skus).zip(second) {
        assert_eq!(block.sku, [sku, 0, 0]);
        assert_eq!(block.hardware, Hardware { mac: 0x42, rf: 0x10d });
        assert_eq!(block.version, 0xe1c4_fa8e);
        assert_eq!(block.payloads.iter().map(|p| p.len()).collect::<Vec<_>>(), [1_656, second]);
    }
}

#[test]
fn the_platform_nvm_block_is_chosen_by_sku_and_hardware() {
    let pnvm = Pnvm::parse(AX210_PNVM).expect("decodes");
    let ax210 = Hardware { mac: 0x42, rf: 0x10d };
    assert_eq!(pnvm.choose([0; 3], ax210), Choice::Skip);
    assert_eq!(pnvm.choose([0x0045_10d0, 0, 0], ax210), Choice::Block(&pnvm.blocks[2]));
    assert_eq!(pnvm.choose([0x0045_10d0, 0, 0], Hardware { mac: 0x34, rf: 0x10a }), Choice::None);
    assert_eq!(pnvm.choose([0x0045_10d1, 0, 0], ax210), Choice::None);
}

#[test]
fn a_platform_nvm_record_outside_a_block_or_of_an_unknown_kind_is_refused() {
    let record = |kind: u32, contents: &[u8]| {
        let mut r = kind.to_le_bytes().to_vec();
        r.extend_from_slice(&u32::try_from(contents.len()).unwrap().to_le_bytes());
        r.extend_from_slice(contents);
        r
    };
    assert_eq!(Pnvm::parse(&record(pnvm::kind::VERSION, &[0; 4])), Err(pnvm::Refused::Outside { at: 0 }));
    let mut file = record(pnvm::kind::SKU, &[0; 12]);
    file.extend(record(70, &[0; 4]));
    assert_eq!(Pnvm::parse(&file), Err(pnvm::Refused::Unknown { kind: 70, at: 20 }));
    let file = record(pnvm::kind::SKU, &[0; 12]);
    assert_eq!(Pnvm::parse(&file), Err(pnvm::Refused::Missing { kind: pnvm::kind::HARDWARE, block: 0 }));
    assert_eq!(Pnvm::parse(&[]), Err(pnvm::Refused::Empty));
}

#[test]
fn a_part_is_served_only_as_the_hardware_its_file_is_for() {
    // The AX200's registers as the AMD bench read them, and the AX210's
    // values as Intel's definitions name them (§1).
    let ax200_read = Hardware::read(0x0000_0340, 0x0010_a100);
    let ax210_read = Hardware::read(0x0000_0420, 0x0010_d000);
    assert_eq!(ax200_read, Hardware { mac: 0x34, rf: 0x10a });
    assert_eq!(ax210_read, Hardware { mac: 0x42, rf: 0x10d });
    let (a, b) = (ax200(), ax210());
    assert_eq!(Part::of(0x8086, 0x2723), Some(Part::Ax200));
    assert_eq!(Part::of(0x8086, 0x2725), Some(Part::Ax210));
    assert_eq!(Part::of(0x8086, 0x2726), None);
    assert_eq!(Part::Ax200.serve(ax200_read, &a), Ok(()));
    assert_eq!(Part::Ax210.serve(ax210_read, &b), Ok(()));
    // The other part's file, whatever the card reads as.
    assert_eq!(Part::Ax200.serve(ax210_read, &b), Err(NotServed::File { part: Part::Ax200, file: b.hardware }));
    // Its own file, and a card reading as other hardware.
    assert_eq!(Part::Ax200.serve(ax210_read, &a), Err(NotServed::Hardware { part: Part::Ax200, read: ax210_read }));
    assert_eq!(Part::Ax200.firmware(), "iwlwifi-cc-a0-77.ucode");
    assert_eq!(Part::Ax210.pnvm(), Some("iwlwifi-ty-a0-gf-a0.pnvm"));
}

// ---- hostile containers ----

/// A header for a file of the container's format, with `name`.
fn header() -> Vec<u8> {
    let mut h = vec![0; container::HEADER_BYTES];
    h[4..8].copy_from_slice(&container::MAGIC.to_le_bytes());
    h
}

fn record(kind: u32, contents: &[u8]) -> Vec<u8> {
    let mut r = kind.to_le_bytes().to_vec();
    r.extend_from_slice(&u32::try_from(contents.len()).unwrap().to_le_bytes());
    r.extend_from_slice(contents);
    r.resize(r.len().next_multiple_of(4), 0);
    r
}

fn word(v: u32) -> [u8; 4] {
    v.to_le_bytes()
}

fn section(load: u32, len: usize) -> Vec<u8> {
    let mut contents = word(load).to_vec();
    contents.resize(4 + len, 0xa5);
    record(container::kind::SECTION, &contents)
}

/// Every record a file must carry, with one section per image.
fn minimal() -> Vec<u8> {
    use container::kind;
    let mut f = header();
    for (k, contents) in [
        (kind::VERSION, [word(1), word(2), word(3)].concat()),
        (kind::HARDWARE, [0x34u16.to_le_bytes(), 0x10au16.to_le_bytes()].concat()),
        (kind::FLAGS, word(0).to_vec()),
        (kind::PHY_CONFIG, word(0).to_vec()),
        (kind::PROBE_MAX, word(512).to_vec()),
        (kind::SCAN_CHANNELS, word(67).to_vec()),
        (kind::STATIONS, word(15).to_vec()),
        (kind::CPUS, word(2).to_vec()),
        (kind::LMAC_DEBUG, word(0x4d_ff20).to_vec()),
        (kind::UMAC_DEBUG, [word(0x88_0128), word(0)].concat()),
        (kind::COMMAND_VERSIONS, vec![0x0d, 0x01, 15, 0]),
    ] {
        f.extend(record(k, &contents));
    }
    f.extend(section(0x1000, 8));
    f.extend(section(container::LMAC_END, 4));
    f.extend(section(0x2000, 8));
    f.extend(section(container::UMAC_END, 4));
    f.extend(section(0x3000, 8));
    f
}

#[test]
fn the_minimal_file_decodes() {
    let bytes = minimal();
    let f = Firmware::parse(&bytes).expect("decodes");
    assert_eq!([shape(&f.lmac), shape(&f.umac), shape(&f.paging)], [(1, 8); 3]);
    assert_eq!(f.lmac[0].load, 0x1000);
}

#[test]
fn a_header_of_another_format_or_magic_is_refused() {
    assert_eq!(Firmware::parse(&[0; 87]), Err(Refused::Short));
    let mut f = minimal();
    f[0] = 1;
    assert_eq!(Firmware::parse(&f), Err(Refused::Format(1)));
    let mut f = minimal();
    f[4] = 0;
    assert_eq!(Firmware::parse(&f), Err(Refused::Magic(0x0a4c_5700)));
}

#[test]
fn a_record_running_past_the_end_is_refused() {
    let mut f = minimal();
    let at = f.len();
    f.extend(record(7, &[0; 8]));
    f.truncate(f.len() - 1);
    assert_eq!(Firmware::parse(&f), Err(Refused::Truncated { at }));
    // A record header cut short is a record past the end too.
    let mut f = minimal();
    let at = f.len();
    f.extend([7, 0, 0]);
    assert_eq!(Firmware::parse(&f), Err(Refused::Truncated { at }));
}

#[test]
fn a_last_record_may_end_without_its_padding() {
    let mut f = minimal();
    f.extend(record(7, &[1, 2, 3]));
    f.truncate(f.len() - 1);
    assert!(Firmware::parse(&f).is_ok());
}

#[test]
fn an_unknown_kind_is_refused_by_kind() {
    let mut f = minimal();
    let at = f.len();
    f.extend(record(70, &[0; 4]));
    assert_eq!(Firmware::parse(&f), Err(Refused::Unknown { kind: 70, at }));
}

#[test]
fn a_second_singleton_or_flag_word_is_refused() {
    let mut f = minimal();
    let at = f.len();
    f.extend(record(container::kind::CPUS, &word(2)));
    assert_eq!(Firmware::parse(&f), Err(Refused::Repeated { kind: container::kind::CPUS, at }));
    // A word given twice, even one that sets no bit.
    let mut f = minimal();
    f.extend(record(container::kind::API, &[word(1), word(0)].concat()));
    let at = f.len();
    f.extend(record(container::kind::API, &[word(1), word(4)].concat()));
    assert_eq!(Firmware::parse(&f), Err(Refused::Repeated { kind: container::kind::API, at }));
    let mut f = minimal();
    f.extend(record(container::kind::CAPABILITIES, &[word(8), word(1)].concat()));
    assert_eq!(Firmware::parse(&f), Err(Refused::WordIndex { kind: container::kind::CAPABILITIES, index: 8 }));
}

#[test]
fn a_record_of_the_wrong_length_is_refused() {
    let mut f = minimal();
    let at = f.len();
    f.extend(record(container::kind::FLAGS, &[0; 8]));
    assert_eq!(Firmware::parse(&f), Err(Refused::Length { kind: container::kind::FLAGS, at, len: 8 }));
}

#[test]
fn separators_out_of_order_or_missing_are_refused() {
    let mut f = minimal();
    let at = f.len();
    f.extend(section(container::LMAC_END, 4));
    assert_eq!(Firmware::parse(&f), Err(Refused::Separator { at }));
    let f = minimal();
    // Drop the UMAC separator and the paging section after it, then put the
    // paging section back: the UMAC image is never ended.
    let without_umac_end = f.len() - section(0, 4).len() - section(0, 8).len();
    let mut cut = f[..without_umac_end].to_vec();
    cut.extend(section(0x3000, 8));
    assert_eq!(Firmware::parse(&cut), Err(Refused::Separator { at: cut.len() }));
}

#[test]
fn an_image_without_a_section_or_two_cpus_is_refused() {
    let mut f = minimal();
    f.truncate(f.len() - section(0, 8).len());
    assert_eq!(Firmware::parse(&f), Err(Refused::Empty(Image::Paging)));
    let f = minimal();
    let cpus = record(container::kind::CPUS, &word(2));
    let at = f.windows(cpus.len()).position(|w| w == cpus).expect("the CPU record");
    let mut f = f;
    f[at + 8] = 1;
    assert_eq!(Firmware::parse(&f), Err(Refused::Cpus(1)));
}

#[test]
fn a_missing_record_is_refused_by_kind() {
    let f = minimal();
    let stations = record(container::kind::STATIONS, &word(15));
    let at = f.windows(stations.len()).position(|w| w == stations).expect("the station record");
    let mut cut = f[..at].to_vec();
    cut.extend_from_slice(&f[at + stations.len()..]);
    assert_eq!(Firmware::parse(&cut), Err(Refused::Missing(container::kind::STATIONS)));
}

// ---- commands ----

/// Every command, with the payload length its section asserts.
fn commands() -> Vec<(Command, usize)> {
    let (versions, file) = (Versions::of(&ax210().declared).unwrap(), ax210());
    let scan = ScanRequest::default().encode(&versions, &file).expect("an empty scan request");
    vec![
        (command::init_flags(command::init::NVM_ACCESS), 4),
        (command::nvm_done(), 4),
        (command::nvm_info(), 4),
        (command::transmit_antennas(0b11), 4),
        (command::coexistence(CoexistenceMode::Wifi, 0), 8),
        (command::platform_latency(command::platform::DISCRETE, 0), 8),
        (command::LatencyTolerance { flags: command::LatencyTolerance::ENABLE, ..Default::default() }.encode(), 32),
        (command::temperature_thresholds(0, [0; 8]), 20),
        (command::device_power(0), 4),
        (command::country_update(*b"ZZ", command::COUNTRY_SOURCE_CURRENT), 28),
        (command::ScanConfiguration::default().encode(), 12),
        (command::beacon_filter_off(), 60),
        (scan, 1_940),
        (command::scan_abort(1), 8),
    ]
}

#[test]
fn every_command_is_the_size_its_section_asserts() {
    for (command, len) in commands() {
        assert_eq!(command.payload.len(), len, "{}", command.message.name);
    }
}

#[test]
fn every_command_is_one_the_versions_check_holds() {
    for (command, _) in commands() {
        assert_eq!(command.message.side, Side::Command, "{}", command.message.name);
        assert!(versions::EXCHANGED.contains(&command.message), "{}", command.message.name);
    }
}

#[test]
fn fields_land_where_their_layouts_put_them() {
    let country = command::country_update(*b"US", command::COUNTRY_SOURCE_CURRENT).payload;
    assert_eq!(country[..4], [b'S', b'U', 0x10, 0]);
    let coex = command::coexistence(CoexistenceMode::Wifi, 1 << 3).payload;
    assert_eq!(coex, [3, 0, 0, 0, 8, 0, 0, 0]);
    let ltr = command::LatencyTolerance { values: [1, 2, 3, 4], short_idle_timeout: 5, ..Default::default() }.encode().payload;
    assert_eq!(ltr[12..32], [1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 4, 0, 0, 0, 5, 0, 0, 0]);
    let power = command::device_power(command::power::SAVE_ALLOWED).payload;
    assert_eq!(power, [1, 0, 0, 0]);
    let config = command::ScanConfiguration { transmit_chains: 3, receive_chains: 3, ..Default::default() }.encode().payload;
    assert_eq!(config, [0, 0, 0, 0, 3, 0, 0, 0, 3, 0, 0, 0]);
}

const CHANNEL_6: ScanChannel = ScanChannel { number: 6, band: Band::Ghz2_4, ssids: 0, aps_override: 0, iterations: 1, interval: 0 };
const CHANNEL_36: ScanChannel = ScanChannel { number: 36, band: Band::Ghz5, ssids: 1, aps_override: 0b10, iterations: 1, interval: 0 };

/// The channel entries at the scan request's offset 48.
fn entries(versions: &Versions, file: &Firmware<'_>, channels: &[ScanChannel]) -> Vec<u8> {
    let request = ScanRequest { channels, ..Default::default() };
    let payload = request.encode(versions, file).expect("encodes").payload;
    assert_eq!(payload[45], u8::try_from(channels.len()).unwrap(), "the channel count");
    payload[48..48 + 8 * channels.len()].to_vec()
}

#[test]
fn a_scan_channel_carries_its_band_where_the_declared_version_puts_it() {
    let (a, b) = (ax200(), ax210());
    let (va, vb) = (Versions::of(&a.declared).unwrap(), Versions::of(&b.declared).unwrap());
    // Version 15: the band in byte 5, 2.4 GHz as 1 and 5 GHz as 0, as the
    // AX200 scanned with.
    assert_eq!(entries(&va, &a, &[CHANNEL_6, CHANNEL_36]), [0, 0, 0, 0, 6, 1, 1, 0, 1, 0, 0x20, 0, 36, 0, 1, 0]);
    // Version 17: the band in flags bits 31:30, and the PSD unset.
    assert_eq!(entries(&vb, &b, &[CHANNEL_6, CHANNEL_36]), [0, 0, 0, 0x40, 6, 0x80, 1, 0, 1, 0, 0x20, 0, 36, 0x80, 1, 0]);
}

#[test]
fn a_scan_request_the_file_or_the_layout_cannot_hold_is_refused() {
    let file = ax210();
    let versions = Versions::of(&file.declared).unwrap();
    let many = [CHANNEL_6; 68];
    let refused = |request: ScanRequest<'_>| request.encode(&versions, &file).map(|_| ());
    assert_eq!(refused(ScanRequest { channels: &many, ..Default::default() }), Err(ScanRefused::Channels(68)));
    assert_eq!(refused(ScanRequest { channels: &many[..67], ..Default::default() }), Ok(()));
    let mut fewer = file.clone();
    fewer.scan_channels = 3;
    assert_eq!(
        ScanRequest { channels: &many[..4], ..Default::default() }.encode(&versions, &fewer).map(|_| ()),
        Err(ScanRefused::Channels(4))
    );
    let template = [0; 513];
    let probe = Probe { template: &template, ..Default::default() };
    assert_eq!(refused(ScanRequest { probe, ..Default::default() }), Err(ScanRefused::Probe(513)));
    let past = Segment { offset: 10, len: 3 };
    let probe = Probe { template: &template[..12], common: past, ..Default::default() };
    assert_eq!(refused(ScanRequest { probe, ..Default::default() }), Err(ScanRefused::Segment(past)));
    let long = [0; 33];
    assert_eq!(refused(ScanRequest { ssids: &[&long], ..Default::default() }), Err(ScanRefused::Ssid(33)));
    let wide = ScanChannel { ssids: 1 << 20, ..CHANNEL_6 };
    assert_eq!(refused(ScanRequest { channels: &[wide], ..Default::default() }), Err(ScanRefused::ChannelBits(wide)));
}

#[test]
fn a_scan_requests_lists_land_at_their_offsets() {
    let file = ax200();
    let versions = Versions::of(&file.declared).unwrap();
    let template = [0xee; 30];
    let probe = Probe { template: &template, header: Segment { offset: 0, len: 24 }, common: Segment { offset: 24, len: 6 }, ..Default::default() };
    let request = ScanRequest { probe, ssids: &[b"toy"], short_ssids: &[7], bssids: &[[1, 2, 3, 4, 5, 6]], ..Default::default() };
    let p = request.encode(&versions, &file).unwrap().payload;
    assert_eq!(p[596..600], [0, 0, 24, 0]);
    assert_eq!(p[612..616], [24, 0, 6, 0]);
    assert_eq!(p[616..646], template);
    assert_eq!(p[1128..1130], [1, 1]);
    assert_eq!(p[1132..1137], [0, 3, b't', b'o', b'y']);
    assert_eq!(p[1812..1816], [7, 0, 0, 0]);
    assert_eq!(p[1844..1850], [1, 2, 3, 4, 5, 6]);
}

// ---- notifications ----

#[test]
fn every_notification_is_refused_off_its_asserted_size() {
    let short = |message: versions::Message, expected: usize, got: usize| Err::<(), _>(Malformed::Length { message: message.name, expected, got });
    assert_eq!(Alive::parse(&[0; 143]).map(|_| ()), short(versions::ALIVE, 144, 143));
    assert_eq!(NvmInfo::parse(&[0; 469]).map(|_| ()), short(versions::NVM_INFO_ANSWER, 468, 469));
    assert_eq!(notification::ScanDone::parse(&[0; 12]).map(|_| ()), short(versions::SCAN_DONE, 16, 12));
    assert_eq!(notification::pnvm_loaded(&[0; 8]).map(|_| ()), short(versions::PNVM_LOADED, 4, 8));
    assert_eq!(notification::init_flags_answer(&[]).map(|_| ()), short(versions::INIT_FLAGS, 4, 0));
    assert_eq!(notification::CommandError::parse(&[0; 16]).map(|_| ()), short(versions::COMMAND_ERROR, 20, 16));
    assert_eq!(notification::ThermalShutdown::parse(&[0; 2]).map(|_| ()), short(versions::THERMAL_SHUTDOWN, 4, 2));
    assert_eq!(notification::CountryChange::parse(&[0; 5]).map(|_| ()), short(versions::COUNTRY_CHANGE, 4, 5));
    assert!(Alive::parse(&[0; 144]).is_ok());
    assert!(NvmInfo::parse(&[0; 468]).is_ok());
}

#[test]
fn the_alive_notification_gives_its_error_tables_and_sku() {
    let mut p = vec![0; 144];
    p[0..2].copy_from_slice(&notification::ALIVE_OK.to_le_bytes());
    p[4..8].copy_from_slice(&77u32.to_le_bytes());
    p[20..24].copy_from_slice(&0x004d_ff20u32.to_le_bytes());
    p[68..72].copy_from_slice(&0x0050_0000u32.to_le_bytes());
    p[108..112].copy_from_slice(&0x0088_0128u32.to_le_bytes());
    p[116..120].copy_from_slice(&0x0045_10d0u32.to_le_bytes());
    let alive = Alive::parse(&p).unwrap();
    assert_eq!(alive.status, 0xcafe);
    assert_eq!(alive.lmac[0].major, 77);
    assert_eq!([alive.lmac[0].error_table, alive.lmac[1].error_table, alive.umac_error_table], [0x004d_ff20, 0x0050_0000, 0x0088_0128]);
    assert_eq!(alive.sku, [0x0045_10d0, 0, 0]);
}

#[test]
fn a_country_answer_is_its_header_and_four_bytes_a_channel() {
    // The AX200's: 51 channels, 224 bytes (§10.12).
    let mut p = vec![0; 224];
    p[0] = 1;
    p[4..6].copy_from_slice(&u16::from_be_bytes(*b"00").to_le_bytes());
    p[12] = 7;
    p[16] = 51;
    p[20..22].copy_from_slice(&0x0f01u16.to_le_bytes());
    let answer = CountryAnswer::parse(&p).unwrap();
    assert_eq!((answer.status, answer.country, answer.source), (1, *b"00", 7));
    assert_eq!((answer.channels.len(), answer.channels[0]), (51, 0x0f01));
    assert_eq!(
        CountryAnswer::parse(&p[..220]),
        Err(Malformed::Length { message: "country update answer", expected: 224, got: 220 })
    );
    let mut over = vec![0; 20 + 4 * 111];
    over[16] = 111;
    assert_eq!(CountryAnswer::parse(&over), Err(Malformed::Count { message: "country update answer", count: 111, most: 110 }));
}

#[test]
fn an_iteration_is_its_header_and_eight_bytes_a_channel() {
    let mut p = vec![0; 16 + 8 * 38];
    p[4] = 38;
    p[5] = notification::SCAN_COMPLETED;
    p[16] = 1;
    p[17] = 1;
    p[20..24].copy_from_slice(&110_000u32.to_le_bytes());
    let done = IterationDone::parse(&p).unwrap();
    assert_eq!((done.status, done.results.len()), (1, 38));
    assert_eq!(done.results[0], notification::ChannelResult { channel: 1, band: 1, probe_status: 0, probes_not_sent: 0, duration: 110_000 });
    assert_eq!(
        IterationDone::parse(&p[..p.len() - 1]),
        Err(Malformed::Length { message: "scan iteration done", expected: 320, got: 319 })
    );
}

/// A received-frame payload: a descriptor of `form`, a frame of `len` bytes
/// and `trailing` bytes after it.
fn received(form: ReceivedForm, len: u16, trailing: usize) -> Vec<u8> {
    let d = form.descriptor();
    let mut p = vec![0; d + usize::from(len) + trailing];
    p[0..2].copy_from_slice(&len.to_le_bytes());
    p[3] = 0x0c;
    p[12..16].copy_from_slice(&0x0000_1007u32.to_le_bytes());
    let rate = match form {
        ReceivedForm::V4 => 28,
        ReceivedForm::V5 => 36,
    };
    p[rate + 4] = 50;
    p[rate + 5] = 60;
    p[rate + 6] = 6;
    p[d] = 0x80;
    p
}

#[test]
fn a_received_frame_is_exactly_its_mpdu_length_after_its_descriptor() {
    for form in [ReceivedForm::V4, ReceivedForm::V5] {
        // The AX200's beacon of its AP: 241 bytes, then 7 more in the packet.
        let p = received(form, 241, 7);
        let frame = Received::parse(&p, form).unwrap();
        assert_eq!(frame.frame.len(), 241, "{form:?}");
        assert_eq!(frame.frame[0], 0x80);
        assert_eq!((frame.status, frame.mac_flags2, frame.energy, frame.channel), (0x1007, 0x0c, [50, 60], 6), "{form:?}");
        let cut = &p[..form.descriptor() + 240];
        assert_eq!(
            Received::parse(cut, form),
            Err(Malformed::Length { message: "received frame", expected: form.descriptor() + 241, got: cut.len() })
        );
    }
    assert_eq!(ReceivedForm::V4.descriptor(), 48);
    assert_eq!(ReceivedForm::V5.descriptor(), 64);
}

#[test]
fn an_empty_table_names_nothing() {
    use crate::errors::{self, Lmac, Umac};
    assert_eq!(Lmac::read(&[0; errors::LMAC_DWORDS]), None);
    assert_eq!(Umac::read(&[0; errors::UMAC_DWORDS]), None);
}

#[test]
fn an_error_is_named_with_its_bits_31_to_28_masked() {
    use crate::errors::name;
    assert_eq!(name(0x0000_0071), "UMAC fatal");
    assert_eq!(name(0xf000_0071), "UMAC fatal");
    assert_eq!(name(0x1000_0034), "watchdog NMI");
    assert_eq!(name(0x0100_0071), "advanced system assert");
    assert_eq!(name(0x2010_02fd), "advanced system assert");
}

#[test]
fn the_profile_maps_a_part_s_entries_to_their_channels() {
    for part in [Part::Ax200, Part::Ax210] {
        assert_eq!(part.profile_channel(0), Some((Band::Ghz2_4, 1)));
        assert_eq!(part.profile_channel(13), Some((Band::Ghz2_4, 14)));
        assert_eq!(part.profile_channel(14), Some((Band::Ghz5, 36)));
        assert_eq!(part.profile_channel(41), Some((Band::Ghz5, 144)));
        assert_eq!(part.profile_channel(42), Some((Band::Ghz5, 149)));
        assert_eq!(part.profile_channel(50), Some((Band::Ghz5, 181)));
    }
    assert_eq!(Part::Ax200.profile_channel(51), None);
    assert_eq!(Part::Ax210.profile_channel(51), Some((Band::Ghz6, 1)));
    assert_eq!(Part::Ax210.profile_channel(109), Some((Band::Ghz6, 233)));
    assert_eq!(Part::Ax210.profile_channel(110), None);
}
