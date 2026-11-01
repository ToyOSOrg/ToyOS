//! The commands the driver sends through a passive scan (§10), each encoded
//! to its payload and carrying the message it is, so nothing sends one
//! payload under another's identifier, and the command channel's 8-byte
//! header in front of it (§8.2).

use alloc::vec::Vec;

use crate::container::Firmware;
use crate::versions::{self, Message, ScanForm, Versions};

/// One command: the message it is, and its payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    pub message: Message,
    pub payload: Vec<u8>,
}

impl Command {
    /// The command's 8-byte header (§8.2): its identifier and group, its sequence, the payload's
    /// length, a reserved byte and version 0. The sequence is the descriptor's `slot` on command
    /// queue 0, its reserved bits clear. `None` for a payload past what the length field holds.
    pub fn header(&self, slot: u8) -> Option<[u8; 8]> {
        let [l0, l1] = u16::try_from(self.payload.len()).ok()?.to_le_bytes();
        Some([self.message.id.id, self.message.id.group, slot, 0, l0, l1, 0, 0])
    }
}

/// A payload as it is written, field by field.
struct Out(Vec<u8>);

impl Out {
    fn new() -> Out {
        Out(Vec::new())
    }
    fn u8(mut self, v: u8) -> Out {
        self.0.push(v);
        self
    }
    fn u16(mut self, v: u16) -> Out {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn u32(mut self, v: u32) -> Out {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn bytes(mut self, v: &[u8]) -> Out {
        self.0.extend_from_slice(v);
        self
    }
    fn zeros(mut self, n: usize) -> Out {
        self.0.resize(self.0.len().saturating_add(n), 0);
        self
    }
    fn done(self, message: Message) -> Command {
        Command { message, payload: self.0 }
    }
}

/// Init flags (§10.2): what follows the firmware's start.
pub mod init {
    pub const DEBUG_CONFIGURATION: u32 = 1 << 0;
    pub const NVM_ACCESS: u32 = 1 << 1;
    pub const PHY_CONFIGURATION: u32 = 1 << 2;
}

pub fn init_flags(flags: u32) -> Command {
    Out::new().u32(flags).done(versions::INIT_FLAGS)
}

/// NVM done (§10.3): one reserved word.
pub fn nvm_done() -> Command {
    Out::new().u32(0).done(versions::NVM_DONE)
}

/// The NVM-info request (§10.5): one reserved word.
pub fn nvm_info() -> Command {
    Out::new().u32(0).done(versions::NVM_INFO)
}

/// The valid transmit antennas (§10.6): bit 0 chain A, bit 1 chain B.
pub fn transmit_antennas(mask: u32) -> Command {
    Out::new().u32(mask).done(versions::TRANSMIT_ANTENNAS)
}

/// The coexistence mode (§10.7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoexistenceMode {
    Disabled,
    Nw,
    Bluetooth,
    Wifi,
}

/// Coexistence (§10.7): the mode, and the modules' bits.
pub fn coexistence(mode: CoexistenceMode, modules: u32) -> Command {
    let mode = match mode {
        CoexistenceMode::Disabled => 0,
        CoexistenceMode::Nw => 1,
        CoexistenceMode::Bluetooth => 2,
        CoexistenceMode::Wifi => 3,
    };
    Out::new().u32(mode).u32(modules).done(versions::COEXISTENCE)
}

/// Platform latency (§10.8).
pub mod platform {
    pub const DISCRETE: u32 = 1 << 0;
    pub const LOW_LATENCY_CRYSTAL: u32 = 1 << 1;
}

/// Platform latency (§10.8): flags as bits (version 2), and the time the SoC
/// needs for stable power and crystal.
pub fn platform_latency(flags: u32, latency: u32) -> Command {
    Out::new().u32(flags).u32(latency).done(versions::PLATFORM_LATENCY)
}

/// Latency tolerance (§10.9), Intel's 32-byte layout.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LatencyTolerance {
    pub flags: u32,
    pub static_long: u32,
    pub static_short: u32,
    /// Transmit, receive, short idle and long idle, in µs.
    pub values: [u32; 4],
    pub short_idle_timeout: u32,
}

impl LatencyTolerance {
    pub const ENABLE: u32 = 1 << 0;
    pub const UPDATE_VALUES: u32 = 1 << 7;

    pub fn encode(&self) -> Command {
        let [tx, rx, short, long] = self.values;
        Out::new()
            .u32(self.flags)
            .u32(self.static_long)
            .u32(self.static_short)
            .u32(tx)
            .u32(rx)
            .u32(short)
            .u32(long)
            .u32(self.short_idle_timeout)
            .done(versions::LATENCY_TOLERANCE)
    }
}

/// Temperature thresholds (§10.10): a count, then eight 16-bit thresholds.
/// None hands thermal shutdown and back-off to the firmware.
pub fn temperature_thresholds(count: u32, thresholds: [u16; 8]) -> Command {
    let out = Out::new().u32(count);
    thresholds.iter().fold(out, |out, &t| out.u16(t)).done(versions::TEMPERATURE_THRESHOLDS)
}

/// Device power (§10.11).
pub mod power {
    pub const SAVE_ALLOWED: u16 = 1 << 0;
    pub const MEMORY_RETENTION: u16 = 1 << 1;
    pub const NO_SLEEP_UNTIL_D3: u16 = 1 << 7;
    pub const EXTERNAL_32K: u16 = 1 << 12;
}

/// Device power (§10.11): its flags and a reserved half-word.
pub fn device_power(flags: u16) -> Command {
    Out::new().u16(flags).u16(0).done(versions::DEVICE_POWER)
}

/// The source that asks for the current country (§10.12).
pub const COUNTRY_SOURCE_CURRENT: u8 = 0x10;

/// Country update (§10.12): two ASCII letters, `ZZ` for the NVM's default
/// profile and `00` for the world domain, and the source. The OEM test key is
/// zero.
pub fn country_update(country: [u8; 2], source: u8) -> Command {
    let [high, low] = country;
    Out::new().u16(u16::from_be_bytes([high, low])).u8(source).u8(0).u32(0).zeros(20).done(versions::COUNTRY_UPDATE)
}

/// Scan configuration (§10.13), the reduced layout of version 5.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScanConfiguration {
    pub cam: u8,
    pub promiscuous: u8,
    pub transmit_chains: u32,
    pub receive_chains: u32,
}

impl ScanConfiguration {
    pub fn encode(&self) -> Command {
        Out::new()
            .u8(self.cam)
            .u8(self.promiscuous)
            .u8(0)
            .u8(0)
            .u32(self.transmit_chains)
            .u32(self.receive_chains)
            .done(versions::SCAN_CONFIGURATION)
    }
}

/// The beacon filter (§10.14), all zero: filtering off.
pub fn beacon_filter_off() -> Command {
    Out::new().zeros(60).done(versions::BEACON_FILTER)
}

/// Scan abort (§10.17): the scan's identifier and a reserved word.
pub fn scan_abort(id: u32) -> Command {
    Out::new().u32(id).u32(0).done(versions::SCAN_ABORT)
}

/// A channel's band, in the PHY's encoding (§10.15).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Band {
    Ghz2_4,
    Ghz5,
    Ghz6,
}

impl Band {
    fn phy(self) -> u8 {
        match self {
            Band::Ghz5 => 0,
            Band::Ghz2_4 => 1,
            Band::Ghz6 => 2,
        }
    }
}

/// One channel of a scan request (§10.15).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanChannel {
    pub number: u8,
    pub band: Band,
    /// Bit `i` directs the request's SSID `i` here; bits 19:0.
    pub ssids: u32,
    /// Bit `i` uses the request's APs override `i`; bits 1:0.
    pub aps_override: u8,
    pub iterations: u8,
    pub interval: u8,
}

/// A schedule entry: the interval in seconds, and the iterations, `0xff` for
/// ever.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Schedule {
    pub interval: u16,
    pub iterations: u8,
}

/// Where one part of the probe template is in it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Segment {
    pub offset: u16,
    pub len: u16,
}

/// The probe request template, and its parts: the MAC header, one part per
/// band (2.4, 5 and 6 GHz), and the part common to them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Probe<'a> {
    pub template: &'a [u8],
    pub header: Segment,
    pub bands: [Segment; 3],
    pub common: Segment,
}

/// The scan request (§10.15), Intel's one layout for versions 14 to 17.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScanRequest<'a> {
    /// Type in bits 7:0, sequence from bit 8.
    pub id: u32,
    pub out_of_channel_priority: u32,
    /// [`scan`]'s bits.
    pub flags: u16,
    pub mac: u8,
    /// Per LMAC.
    pub active_dwell: [u8; 2],
    /// The default AP counts for 2.4 GHz, 5 GHz and the social channels.
    pub adaptive_dwell: [u8; 3],
    pub flags2: u8,
    pub adaptive_dwell_budget: u16,
    pub max_out_of_channel: [u32; 2],
    pub suspend: [u32; 2],
    pub priority: u32,
    pub passive_dwell: [u8; 2],
    pub fragments: [u8; 2],
    pub channel_flags: u8,
    pub aps_override: [u8; 2],
    pub channels: &'a [ScanChannel],
    pub schedules: [Schedule; 2],
    pub delay: u16,
    pub probe: Probe<'a>,
    pub ssids: &'a [&'a [u8]],
    pub short_ssids: &'a [u32],
    pub bssids: &'a [[u8; 6]],
}

/// The scan request's general flags (§10.15).
pub mod scan {
    pub const PERIODIC: u16 = 1 << 0;
    pub const PASS_ALL: u16 = 1 << 1;
    pub const NOTIFY_ITERATION: u16 = 1 << 2;
    pub const ADAPTIVE_DWELL: u16 = 1 << 7;
    pub const ALL_PASSIVE: u16 = 1 << 11;
    /// The channel flags' bit that keeps the request's channel order.
    pub const CHANNEL_ORDER: u8 = 1 << 5;
}

/// The layout's room for each list (§10.15).
pub const SCAN_CHANNELS: usize = 67;
pub const PROBE_BYTES: usize = 512;
pub const SSIDS: usize = 20;
pub const SSID_BYTES: usize = 32;
/// An element ID, a length and the SSID's room.
const SSID_ENTRY: usize = 34;
pub const SHORT_SSIDS: usize = 8;
pub const BSSIDS: usize = 16;
const SSID_ELEMENT: u8 = 0;
const PSD_UNSET: u8 = 0x80;

/// Why a scan request cannot be encoded for this file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanRefused {
    /// More channels than the file's scan-channel count or the layout holds.
    Channels(usize),
    /// A template longer than the file's probe maximum or the layout holds.
    Probe(usize),
    /// A segment reaching past the template.
    Segment(Segment),
    Ssids(usize),
    Ssid(usize),
    ShortSsids(usize),
    Bssids(usize),
    /// A channel directing an SSID past bit 19, or an override past bit 1.
    ChannelBits(ScanChannel),
}

fn count(n: usize) -> u8 {
    u8::try_from(n).unwrap_or(u8::MAX)
}

impl ScanRequest<'_> {
    pub fn encode(&self, versions: &Versions, file: &Firmware<'_>) -> Result<Command, ScanRefused> {
        let channel_room = usize::try_from(file.scan_channels).unwrap_or(0).min(SCAN_CHANNELS);
        if self.channels.len() > channel_room {
            return Err(ScanRefused::Channels(self.channels.len()));
        }
        let template = self.probe.template;
        if template.len() > usize::try_from(file.probe_max).unwrap_or(0).min(PROBE_BYTES) {
            return Err(ScanRefused::Probe(template.len()));
        }
        let [band_24, band_5, band_6] = self.probe.bands;
        let segments = [self.probe.header, band_24, band_5, band_6, self.probe.common];
        if let Some(&s) = segments.iter().find(|s| usize::from(s.offset).saturating_add(usize::from(s.len)) > template.len()) {
            return Err(ScanRefused::Segment(s));
        }
        match () {
            _ if self.ssids.len() > SSIDS => return Err(ScanRefused::Ssids(self.ssids.len())),
            _ if self.short_ssids.len() > SHORT_SSIDS => return Err(ScanRefused::ShortSsids(self.short_ssids.len())),
            _ if self.bssids.len() > BSSIDS => return Err(ScanRefused::Bssids(self.bssids.len())),
            _ => {}
        }
        if let Some(ssid) = self.ssids.iter().find(|s| s.len() > SSID_BYTES) {
            return Err(ScanRefused::Ssid(ssid.len()));
        }
        if let Some(&c) = self.channels.iter().find(|c| c.ssids >> 20 != 0 || c.aps_override >> 2 != 0) {
            return Err(ScanRefused::ChannelBits(c));
        }

        let [active_low, active_high] = self.active_dwell;
        let [adwell_24, adwell_5, adwell_social] = self.adaptive_dwell;
        let [passive_low, passive_high] = self.passive_dwell;
        let [fragments_low, fragments_high] = self.fragments;
        let [override_0, override_1] = self.aps_override;
        let mut out = Out::new()
            .u32(self.id)
            .u32(self.out_of_channel_priority)
            .u16(self.flags)
            .u8(0)
            .u8(self.mac)
            .u8(active_low)
            .u8(active_high)
            .u8(adwell_24)
            .u8(adwell_5)
            .u8(adwell_social)
            .u8(self.flags2)
            .u16(self.adaptive_dwell_budget);
        for word in self.max_out_of_channel.iter().chain(&self.suspend) {
            out = out.u32(*word);
        }
        out = out
            .u32(self.priority)
            .u8(passive_low)
            .u8(passive_high)
            .u8(fragments_low)
            .u8(fragments_high)
            .u8(self.channel_flags)
            .u8(count(self.channels.len()))
            .u8(override_0)
            .u8(override_1);
        for channel in self.channels {
            out = channel_entry(out, channel, versions.scan);
        }
        out = out.zeros(SCAN_CHANNELS.saturating_sub(self.channels.len()).saturating_mul(8));
        for schedule in self.schedules {
            out = out.u16(schedule.interval).u8(schedule.iterations).u8(0);
        }
        out = out.u16(self.delay).u16(0);
        for segment in segments {
            out = out.u16(segment.offset).u16(segment.len);
        }
        out = out
            .bytes(template)
            .zeros(PROBE_BYTES.saturating_sub(template.len()))
            .u8(count(self.short_ssids.len()))
            .u8(count(self.bssids.len()))
            .u16(0);
        for ssid in self.ssids {
            out = out.u8(SSID_ELEMENT).u8(count(ssid.len())).bytes(ssid).zeros(SSID_BYTES.saturating_sub(ssid.len()));
        }
        out = out.zeros(SSIDS.saturating_sub(self.ssids.len()).saturating_mul(SSID_ENTRY));
        for short in self.short_ssids {
            out = out.u32(*short);
        }
        out = out.zeros(SHORT_SSIDS.saturating_sub(self.short_ssids.len()).saturating_mul(4));
        for bssid in self.bssids {
            out = out.bytes(bssid);
        }
        out = out.zeros(BSSIDS.saturating_sub(self.bssids.len()).saturating_mul(6));
        Ok(out.done(versions::SCAN_REQUEST))
    }
}

/// One 8-byte channel entry. Below version 17 the band is byte 5; at 17 it is
/// flags bits 31:30 and byte 5 is the highest PSD, left unset. That the
/// flags carry the PHY's encoding is the specification's inference, open
/// until the AX210 scans (§14, item 3).
fn channel_entry(out: Out, channel: &ScanChannel, form: ScanForm) -> Out {
    let override_bits = u32::from(channel.aps_override) << 20;
    let (flags, fifth) = match form {
        ScanForm::BandByte => (channel.ssids | override_bits, channel.band.phy()),
        ScanForm::BandInFlags => (channel.ssids | override_bits | u32::from(channel.band.phy()) << 30, PSD_UNSET),
    };
    out.u32(flags).u8(channel.number).u8(fifth).u8(channel.iterations).u8(channel.interval)
}
