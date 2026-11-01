//! W3's recorded bench traces answered (the AX2xx interface specification, revision 2, §15): the
//! AX200's Run13, from the alive notification through a passive scan, and Run11, through a
//! command the firmware refused. Each device line's payload is decoded twice, as recorded and
//! with every bit the run did not record complemented, and the decisions taken from it must not
//! change: they rest on recorded bits alone. Each command line the specification sends is then
//! built from those decisions and the file, and must be the run's bytes, header and payload:
//! Run13's commands 0 to 11 and its scan request, and Run11's 0 to 8. Commands 12 to 14 are the
//! demo's, which this specification does not send (§15.3), and are not built; the scan request's
//! probe template and its segments are the caller's input, as the template is no layout of this
//! crate's, and are taken from the run.
//!
//! `tests/traces/` holds the specification's `spec/traces/` files unmodified, from
//! `ToyOSOrg/evidence` branch `pr-876` at 9b841a5fd312a0cd6ae341dc8c0dec1e3f6fa204.

use toyos_ax2xx::command::{self, Band, CoexistenceMode, LatencyTolerance, Probe, ScanChannel, ScanConfiguration, ScanRequest, Schedule, Segment};
use toyos_ax2xx::container::Firmware;
use toyos_ax2xx::errors::{self, Lmac, Umac};
use toyos_ax2xx::notification::{self, Alive, CountryAnswer, CountryChange, IterationDone, NvmInfo, ScanDone};
use toyos_ax2xx::part::Part;
use toyos_ax2xx::versions::Versions;

const RUN13: &str = include_str!("traces/ax200-run13.trace");
const RUN11: &str = include_str!("traces/ax200-run11.trace");
const AX200: &[u8] = include_bytes!("../../firmware/iwlwifi-cc-a0-77.ucode");

/// One line of a trace (§15.1).
enum Line {
    /// A command: its index, and its header and payload.
    Command { idx: usize, bytes: Vec<u8> },
    /// A packet the device wrote: what it is, its bytes from the length-and-flags word on, and
    /// the mask of the bits the run recorded.
    Device { what: String, bytes: Vec<u8>, recorded: Vec<u8> },
    /// A device-memory table the host read.
    Table { what: String, dwords: Vec<u32> },
    /// Packets counted and not recorded, or a line saying what did not come.
    Unrecorded,
}

fn hex(text: &str) -> Vec<u8> {
    (0..text.len()).step_by(2).map(|at| u8::from_str_radix(&text[at..at + 2], 16).expect("hex")).collect()
}

fn lines(trace: &str) -> Vec<Line> {
    trace
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| {
            let words: Vec<&str> = l.split_whitespace().collect();
            match words[0] {
                "H" => Line::Command { idx: words[1].parse().unwrap(), bytes: hex(words[4]) },
                "D" => match words.iter().position(|w| w.starts_with("rec=")) {
                    Some(at) => Line::Device { what: words[1..at - 2].join(" "), bytes: hex(words[at - 1]), recorded: hex(&words[at][4..]) },
                    None => Line::Unrecorded,
                },
                "E" => {
                    let from = words.iter().position(|w| w.starts_with("Run")).unwrap() + 1;
                    Line::Table { what: words[1].to_string(), dwords: words[from..].iter().map(|w| u32::from_str_radix(w, 16).unwrap()).collect() }
                }
                other => panic!("a trace line of kind {other}"),
            }
        })
        .collect()
}

/// A device packet's payload: past its length-and-flags word and its header (§9.4).
fn payload(bytes: &[u8]) -> &[u8] {
    let length = usize::try_from(u32::from_le_bytes(bytes[..4].try_into().unwrap()) & 0x3fff).unwrap();
    &bytes[8..4 + length]
}

/// The packet as recorded, and with every unrecorded bit past its length-and-flags word
/// complemented. That word is framing, kept as the trace fills it; on five lines the run recorded
/// none of it.
fn both(bytes: &[u8], recorded: &[u8]) -> [Vec<u8>; 2] {
    let flipped = bytes[..4].iter().copied().chain(bytes[4..].iter().zip(&recorded[4..]).map(|(b, r)| b ^ !r)).collect();
    [bytes.to_vec(), flipped]
}

/// `decide` taken of a device line as recorded and as complemented: the same decision both
/// times, which is returned.
fn decided<T: PartialEq + std::fmt::Debug>(bytes: &[u8], recorded: &[u8], decide: impl Fn(&[u8]) -> T) -> T {
    let [as_recorded, complemented] = both(bytes, recorded);
    let decision = decide(payload(&as_recorded));
    assert_eq!(decide(payload(&complemented)), decision, "a decision rests on a bit the run did not record");
    decision
}

/// The command a driver writes for `idx`, at slot `idx`, as the run counted.
fn written(idx: usize, command: &command::Command) -> Vec<u8> {
    let mut bytes = command.header(u8::try_from(idx).unwrap()).expect("a payload the header's length holds").to_vec();
    bytes.extend_from_slice(&command.payload);
    bytes
}

/// What the alive notification decides on the AX200: that the firmware runs, and where its
/// error tables are. Its SKU chooses a platform-NVM block, which only the AX210 is handed (§7.4).
fn alive(payload: &[u8]) -> (u16, u32, u32) {
    let alive = Alive::parse(payload).expect("an alive notification");
    (alive.status, alive.lmac[0].error_table, alive.umac_error_table)
}

/// What the NVM-info answer decides on the AX200: the transmit and receive chains, whether LAR
/// is on, and the valid bit of each profile entry the part has.
fn nvm(payload: &[u8]) -> (u32, u32, bool, Vec<bool>) {
    let nvm = NvmInfo::parse(payload).expect("an NVM-info answer");
    let profile = &nvm.profile[..Part::Ax200.profile_channels()];
    (nvm.transmit_chains, nvm.receive_chains, nvm.lar, profile.iter().map(|e| e & 1 != 0).collect())
}

/// What the country answer decides: its status and the channels it makes valid.
fn country(payload: &[u8]) -> (u32, Vec<(Band, u8)>) {
    let answer = CountryAnswer::parse(payload).expect("a country answer");
    let valid = answer.channels.iter().enumerate().filter(|(_, flags)| **flags & 1 != 0).filter_map(|(at, _)| Part::Ax200.profile_channel(at)).collect();
    (answer.status, valid)
}

/// The run's commands, by index.
fn commands(lines: &[Line]) -> Vec<(usize, Vec<u8>)> {
    lines.iter().filter_map(|l| match l { Line::Command { idx, bytes } => Some((*idx, bytes.clone())), _ => None }).collect()
}

/// The device lines named `what`, in order.
fn device<'a>(lines: &'a [Line], what: &str) -> Vec<(&'a [u8], &'a [u8])> {
    lines
        .iter()
        .filter_map(|l| match l {
            Line::Device { what: w, bytes, recorded } if w == what => Some((bytes.as_slice(), recorded.as_slice())),
            _ => None,
        })
        .collect()
}

/// The commands a driver following the specification writes through the runtime
/// configuration (§4.2, §10.2 to §10.14), from the file and the answers decided so far.
fn configuration(file: &Firmware<'_>, transmit: u32, receive: u32, lar: bool) -> Vec<(usize, command::Command)> {
    let transmit_antennas = (file.phy_config >> 16) & 0xf & transmit;
    let receive_antennas = (file.phy_config >> 20) & 0xf & receive;
    let mut sent = vec![
        (0, command::init_flags(command::init::NVM_ACCESS)),
        (1, command::nvm_done()),
        (2, command::nvm_info()),
        (3, command::transmit_antennas(transmit_antennas)),
        (4, command::coexistence(CoexistenceMode::Wifi, 0)),
        (5, command::platform_latency(command::platform::DISCRETE, 0)),
        // Device control 2 enabled LTR on the AMD bench's AX200 (§2).
        (6, LatencyTolerance { flags: LatencyTolerance::ENABLE, ..Default::default() }.encode()),
        // Capability 74: the firmware owns thermal shutdown (§10.10).
        (7, command::temperature_thresholds(0, [0; 8])),
        // Awake until W8 (§10.11).
        (8, command::device_power(0)),
    ];
    assert!(file.capabilities.has(74), "the firmware owns thermal shutdown");
    // LAR on, capability 1 and API bit 9: `ZZ`, asking for the current country (§10.12).
    assert!(lar && file.capabilities.has(1) && file.api.has(9));
    sent.push((9, command::country_update(*b"ZZ", command::COUNTRY_SOURCE_CURRENT)));
    sent.push((10, ScanConfiguration { transmit_chains: transmit_antennas, receive_chains: receive_antennas, ..Default::default() }.encode()));
    sent.push((11, command::beacon_filter_off()));
    sent
}

#[test]
fn run13_is_answered_through_its_passive_scan() {
    let file = Firmware::parse(AX200).expect("the AX200's pinned file");
    let versions = Versions::of(&file.declared).expect("its declarations");
    let lines = lines(RUN13);
    let ran = commands(&lines);

    let (bytes, recorded) = device(&lines, "alive")[0];
    let (status, lmac_table, umac_table) = decided(bytes, recorded, alive);
    assert_eq!((status, lmac_table, umac_table), (notification::ALIVE_OK, file.lmac_error_table, file.umac_error_table));

    let answers = device(&lines, "answer");
    assert_eq!(decided(answers[0].0, answers[0].1, |p| notification::init_flags_answer(p).unwrap()), 0, "init flags");
    let (transmit, receive, lar, valid) = decided(answers[2].0, answers[2].1, nvm);
    assert_eq!(valid.iter().filter(|v| **v).count(), 38, "the NVM's 38 valid channels (§10.5)");

    for (idx, command) in configuration(&file, transmit, receive, lar) {
        let run = &ran.iter().find(|(i, _)| *i == idx).unwrap().1;
        assert_eq!(&written(idx, &command), run, "command {idx}, {}", command.message.name);
    }

    // The country answer, then the scan over the channels it makes valid (§15.3).
    let (status, channels) = decided(answers[9].0, answers[9].1, country);
    assert_eq!(status, 1, "the same profile");
    assert!(channels.len() <= usize::try_from(file.scan_channels).unwrap());
    let channels: Vec<ScanChannel> = channels
        .iter()
        .map(|&(band, number)| ScanChannel { number, band, ssids: 0, aps_override: 0, iterations: 1, interval: 0 })
        .collect();
    let run = &ran.iter().find(|(i, _)| *i == 15).unwrap().1;
    let at = |n: usize| u16::from_le_bytes([run[8 + n], run[9 + n]]);
    let segment = |n: usize| Segment { offset: at(n), len: at(n + 2) };
    let (header, bands, common) = (segment(596), [segment(600), segment(604), segment(608)], segment(612));
    let template = &run[8 + 616..8 + 616 + usize::from(common.offset + common.len)];
    let request = ScanRequest {
        out_of_channel_priority: 6,
        flags: command::scan::PASS_ALL | command::scan::NOTIFY_ITERATION | command::scan::ADAPTIVE_DWELL | command::scan::ALL_PASSIVE,
        active_dwell: [10, 10],
        adaptive_dwell: [2, 8, 10],
        adaptive_dwell_budget: 300,
        priority: 6,
        passive_dwell: [110, 110],
        channel_flags: command::scan::CHANNEL_ORDER,
        aps_override: [10, 2],
        channels: &channels,
        schedules: [Schedule { interval: 0, iterations: 1 }, Schedule::default()],
        probe: Probe { template, header, bands, common },
        ..Default::default()
    };
    let scan = request.encode(&versions, &file).expect("a scan request this file holds");
    assert_eq!(&written(15, &scan), run, "the scan request");

    let (bytes, recorded) = device(&lines, "scan iteration done")[0];
    let done = decided(bytes, recorded, |p| {
        let done = IterationDone::parse(p).expect("an iteration");
        (done.status, done.results.len())
    });
    assert_eq!(done, (notification::SCAN_COMPLETED, 38));
    let (bytes, recorded) = device(&lines, "scan done")[0];
    assert_eq!(decided(bytes, recorded, |p| ScanDone::parse(p).map(|d| (d.status, d.ebs_status))), Ok((notification::SCAN_COMPLETED, 3)));
    let (bytes, recorded) = device(&lines, "country change")[0];
    assert_eq!(decided(bytes, recorded, |p| CountryChange::parse(p).map(|c| (c.country, c.source))), Ok((*b"NL", 5)));
}

#[test]
fn run11_names_the_command_the_firmware_refused() {
    let file = Firmware::parse(AX200).expect("the AX200's pinned file");
    let lines = lines(RUN11);
    let ran = commands(&lines);
    let answers = device(&lines, "answer");
    let (transmit, receive, lar, _) = decided(answers[2].0, answers[2].1, nvm);
    let ours = configuration(&file, transmit, receive, lar);
    for (idx, command) in ours.iter().filter(|(idx, _)| *idx < 9) {
        assert_eq!(&written(*idx, command), &ran.iter().find(|(i, _)| i == idx).unwrap().1, "command {idx}");
    }
    // The demo's country update was 32 bytes; the specification's is 28, its first 28.
    let country = &ours.iter().find(|(idx, _)| *idx == 9).unwrap().1;
    let refused = &ran.iter().find(|(idx, _)| *idx == 9).unwrap().1;
    assert_eq!((country.payload.len(), refused.len() - 8), (28, 32));
    assert_eq!(&country.payload[..], &refused[8..36]);

    // The run read each table's first 24 dwords.
    let table = |what: &str| lines.iter().find_map(|l| match l { Line::Table { what: w, dwords } if w == what => Some(dwords.clone()), _ => None }).unwrap();
    let lmac = Lmac::read(&table("LMAC")[..errors::LMAC_DWORDS].try_into().unwrap()).expect("an LMAC table");
    assert_eq!((lmac.valid, errors::name(lmac.error), lmac.hardware_version), (6, "UMAC fatal", 0x340));
    let umac = Umac::read(&table("UMAC")[..errors::UMAC_DWORDS].try_into().unwrap()).expect("a UMAC table");
    // The refused command's first dword, the length its layout has, and the length it came with.
    let header = u32::from_le_bytes(refused[..4].try_into().unwrap());
    assert_eq!(umac.data, [header, 28, 32]);
    assert_eq!(umac.last_command, header);
    assert_eq!(umac.data[1], u32::try_from(country.payload.len()).unwrap(), "the firmware's length is this crate's layout's");
    assert_eq!((umac.major, umac.minor), (file.version.major, file.version.minor));
}
