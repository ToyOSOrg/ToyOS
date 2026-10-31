//! What the firmware sends through a passive scan (§7.4, §7.5, §10, §11.1,
//! §12.3, §12.5), decoded from a packet's payload and refused by name when it
//! is not its layout's length. The device is on the far side of a trust
//! boundary: no length it writes is believed past the payload it is in.

use alloc::vec::Vec;

use crate::bytes::{array, u8_at, u16_at, u32_at, u64_at};
use crate::versions::{self, Message, ReceivedForm};

/// A payload that is not its message's layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Malformed {
    /// Not the length the layout gives for the counts the payload states.
    Length { message: &'static str, expected: usize, got: usize },
    /// A count past the most the layout holds.
    Count { message: &'static str, count: u32, most: usize },
}

/// `payload` when it is exactly `len` bytes.
fn exactly(message: Message, payload: &[u8], len: usize) -> Result<&[u8], Malformed> {
    match payload.len() == len {
        true => Ok(payload),
        false => Err(Malformed::Length { message: message.name, expected: len, got: payload.len() }),
    }
}

/// A field inside a payload already held to its length.
fn word(p: &[u8], at: usize) -> u32 {
    u32_at(p, at).unwrap_or_default()
}
fn half(p: &[u8], at: usize) -> u16 {
    u16_at(p, at).unwrap_or_default()
}
fn byte(p: &[u8], at: usize) -> u8 {
    u8_at(p, at).unwrap_or_default()
}

/// The alive notification's status that says the firmware runs (§7.5).
pub const ALIVE_OK: u16 = 0xcafe;
pub const ALIVE_BYTES: usize = 144;

/// One LMAC's part of the alive notification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lmac {
    pub major: u32,
    pub minor: u32,
    pub error_table: u32,
}

/// The alive notification, version 6 (§7.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Alive {
    /// [`ALIVE_OK`], or `0xdead` on an error.
    pub status: u16,
    /// Bit 0: RF-kill.
    pub flags: u16,
    pub lmac: [Lmac; 2],
    pub umac_major: u32,
    pub umac_minor: u32,
    pub umac_error_table: u32,
    /// What chooses the platform-NVM block (§7.4).
    pub sku: [u32; 3],
}

impl Alive {
    pub fn parse(payload: &[u8]) -> Result<Alive, Malformed> {
        let p = exactly(versions::ALIVE, payload, ALIVE_BYTES)?;
        let lmac = |at: usize| Lmac {
            major: word(p, at),
            minor: word(p, at.saturating_add(4)),
            error_table: word(p, at.saturating_add(16)),
        };
        Ok(Alive {
            status: half(p, 0),
            flags: half(p, 2),
            lmac: [lmac(4), lmac(52)],
            umac_major: word(p, 100),
            umac_minor: word(p, 104),
            umac_error_table: word(p, 108),
            sku: [word(p, 116), word(p, 120), word(p, 124)],
        })
    }
}

/// The NVM channel profile's length: 14 at 2.4 GHz, 37 at 5 GHz and 59 at
/// 6 GHz (§10.5).
pub const PROFILE_CHANNELS: usize = 110;
pub const NVM_INFO_BYTES: usize = 468;

/// The NVM-info answer, version 4 (§10.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NvmInfo {
    /// Bit 0: the OTP is empty.
    pub general_flags: u32,
    pub nvm_version: u16,
    pub board_type: u8,
    pub addresses: u8,
    /// Bit 0 2.4 GHz, 1 5 GHz, 2 11n, 3 11ac, 4 11ax, 5 MIMO disabled.
    pub sku_flags: u32,
    pub transmit_chains: u32,
    pub receive_chains: u32,
    pub lar: bool,
    /// The valid entries of [`NvmInfo::profile`].
    pub channels: u32,
    /// One word per channel, its bits §10.5's.
    pub profile: [u32; PROFILE_CHANNELS],
}

impl NvmInfo {
    pub fn parse(payload: &[u8]) -> Result<NvmInfo, Malformed> {
        let p = exactly(versions::NVM_INFO_ANSWER, payload, NVM_INFO_BYTES)?;
        let mut profile = [0; PROFILE_CHANNELS];
        for (i, entry) in profile.iter_mut().enumerate() {
            *entry = word(p, i.saturating_mul(4).saturating_add(28));
        }
        Ok(NvmInfo {
            general_flags: word(p, 0),
            nvm_version: half(p, 4),
            board_type: byte(p, 6),
            addresses: byte(p, 7),
            sku_flags: word(p, 8),
            transmit_chains: word(p, 12),
            receive_chains: word(p, 16),
            lar: word(p, 20) != 0,
            channels: word(p, 24),
            profile,
        })
    }
}

/// The country update's answer, read at Intel's version-4 layout (§10.12).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CountryAnswer {
    /// 0 new profile, 1 same profile, 2 invalid, 3 NVM disabled, 4 illegal,
    /// 5 low priority.
    pub status: u32,
    pub country: [u8; 2],
    pub capabilities: u16,
    pub source: u8,
    /// Each channel's flags, as the NVM profile's and in its order.
    pub channels: Vec<u16>,
}

impl CountryAnswer {
    pub fn parse(payload: &[u8]) -> Result<CountryAnswer, Malformed> {
        let message = versions::COUNTRY_ANSWER;
        let short = Malformed::Length { message: message.name, expected: 20, got: payload.len() };
        let stated = u32_at(payload, 16).ok_or(short)?;
        let count = usize::try_from(stated)
            .ok()
            .filter(|&n| n <= PROFILE_CHANNELS)
            .ok_or(Malformed::Count { message: message.name, count: stated, most: PROFILE_CHANNELS })?;
        let p = exactly(message, payload, count.saturating_mul(4).saturating_add(20))?;
        Ok(CountryAnswer {
            status: word(p, 0),
            country: half(p, 4).to_be_bytes(),
            capabilities: half(p, 6),
            source: byte(p, 12),
            channels: (0..count).map(|i| half(p, i.saturating_mul(4).saturating_add(20))).collect(),
        })
    }
}

/// The country change notification (§10.12): the firmware asks for a
/// regulatory update for this country.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CountryChange {
    pub country: [u8; 2],
    pub source: u8,
}

impl CountryChange {
    pub fn parse(payload: &[u8]) -> Result<CountryChange, Malformed> {
        let p = exactly(versions::COUNTRY_CHANGE, payload, 4)?;
        Ok(CountryChange { country: half(p, 0).to_be_bytes(), source: byte(p, 2) })
    }
}

/// A scan's status (§10.16).
pub const SCAN_COMPLETED: u8 = 1;
pub const SCAN_ABORTED: u8 = 2;

/// One channel's result in an iteration (§10.16).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChannelResult {
    pub channel: u8,
    /// 0 5 GHz, 1 2.4 GHz.
    pub band: u8,
    pub probe_status: u8,
    pub probes_not_sent: u8,
    /// µs spent on the channel.
    pub duration: u32,
}

/// The scan iteration done notification (§10.16).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IterationDone {
    pub id: u32,
    pub status: u8,
    pub last_channel: u8,
    /// µs, against the MAC the request named.
    pub start_tsf: u64,
    pub results: Vec<ChannelResult>,
}

impl IterationDone {
    pub fn parse(payload: &[u8]) -> Result<IterationDone, Malformed> {
        let message = versions::SCAN_ITERATION_DONE;
        let count = usize::from(u8_at(payload, 4).ok_or(Malformed::Length { message: message.name, expected: 16, got: payload.len() })?);
        let p = exactly(message, payload, count.saturating_mul(8).saturating_add(16))?;
        let result = |i: usize| {
            let at = i.saturating_mul(8).saturating_add(16);
            ChannelResult {
                channel: byte(p, at),
                band: byte(p, at.saturating_add(1)),
                probe_status: byte(p, at.saturating_add(2)),
                probes_not_sent: byte(p, at.saturating_add(3)),
                duration: word(p, at.saturating_add(4)),
            }
        };
        Ok(IterationDone {
            id: word(p, 0),
            status: byte(p, 5),
            last_channel: byte(p, 7),
            start_tsf: u64_at(p, 8).unwrap_or_default(),
            results: (0..count).map(result).collect(),
        })
    }
}

/// The scan done notification (§10.16).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScanDone {
    pub id: u32,
    pub status: u8,
    /// 0 success, 1 failed, 2 channel not found, 3 inactive.
    pub ebs_status: u8,
}

impl ScanDone {
    pub fn parse(payload: &[u8]) -> Result<ScanDone, Malformed> {
        let p = exactly(versions::SCAN_DONE, payload, 16)?;
        Ok(ScanDone { id: word(p, 0), status: byte(p, 6), ebs_status: byte(p, 7) })
    }
}

/// The platform-NVM-loaded notification (§7.4): its loading status.
pub fn pnvm_loaded(payload: &[u8]) -> Result<u32, Malformed> {
    exactly(versions::PNVM_LOADED, payload, 4).map(|p| word(p, 0))
}

/// The init-flags command's answer (§10.2).
pub fn init_flags_answer(payload: &[u8]) -> Result<u32, Malformed> {
    exactly(versions::INIT_FLAGS, payload, 4).map(|p| word(p, 0))
}

/// The command-error notification (§12.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandError {
    pub error_type: u32,
    pub command: u8,
    pub sequence: u16,
    /// Only with error type 2.
    pub service: u32,
    pub tsf: u64,
}

impl CommandError {
    pub fn parse(payload: &[u8]) -> Result<CommandError, Malformed> {
        let p = exactly(versions::COMMAND_ERROR, payload, 20)?;
        Ok(CommandError {
            error_type: word(p, 0),
            command: byte(p, 4),
            sequence: half(p, 6),
            service: word(p, 8),
            tsf: u64_at(p, 12).unwrap_or_default(),
        })
    }
}

/// The thermal shutdown notification, version 2 (§12.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThermalShutdown {
    pub celsius: u16,
    pub sensors: u8,
    /// 0 software, 1 hardware.
    pub scheme: u8,
}

impl ThermalShutdown {
    pub fn parse(payload: &[u8]) -> Result<ThermalShutdown, Malformed> {
        let p = exactly(versions::THERMAL_SHUTDOWN, payload, 4)?;
        Ok(ThermalShutdown { celsius: half(p, 0), sensors: byte(p, 2), scheme: byte(p, 3) })
    }
}

/// A received frame (§11.1): the descriptor's fields W1 and the scan read,
/// and the frame, exactly its MPDU length; bytes past it in the packet are
/// not part of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Received<'a> {
    /// Bits 1:0 address type; bits 7:4 MIC and CRC length in 2-byte words.
    pub mac_flags1: u8,
    /// Bits 4:0 header length in 2-byte words; bit 5 header padding; bit 6
    /// an A-MSDU subframe.
    pub mac_flags2: u8,
    /// Bits 6:0 subframe index; bit 7 last subframe.
    pub amsdu: u8,
    pub phy_info: u16,
    /// Bits 3:0 MAC; 5:4 PHY; 7:6 band.
    pub mac_phy_band: u8,
    /// §11.2's bits.
    pub status: u32,
    /// §11.4's fields.
    pub reorder: u32,
    pub rate: u32,
    pub energy: [u8; 2],
    pub channel: u8,
    pub arrival: u32,
    pub tsf: u64,
    pub frame: &'a [u8],
}

impl ReceivedForm {
    pub const fn descriptor(self) -> usize {
        match self {
            ReceivedForm::V4 => 48,
            ReceivedForm::V5 => 64,
        }
    }
}

impl<'a> Received<'a> {
    pub fn parse(payload: &'a [u8], form: ReceivedForm) -> Result<Received<'a>, Malformed> {
        let name = versions::RECEIVED_FRAME.name;
        let descriptor = form.descriptor();
        let length = usize::from(u16_at(payload, 0).unwrap_or_default());
        let frame = descriptor
            .checked_add(length)
            .and_then(|end| payload.get(descriptor..end))
            .ok_or(Malformed::Length { message: name, expected: descriptor.saturating_add(length), got: payload.len() })?;
        let p = payload;
        // The tail's offsets of the rate, then the energies, channel, arrival
        // and TSF that follow it in both forms.
        let rate = match form {
            ReceivedForm::V4 => 28,
            ReceivedForm::V5 => 36,
        };
        Ok(Received {
            mac_flags1: byte(p, 2),
            mac_flags2: byte(p, 3),
            amsdu: byte(p, 4),
            phy_info: half(p, 5),
            mac_phy_band: byte(p, 7),
            status: word(p, 12),
            reorder: word(p, 16),
            rate: word(p, rate),
            energy: array(p, rate.saturating_add(4)).unwrap_or_default(),
            channel: byte(p, rate.saturating_add(6)),
            arrival: word(p, rate.saturating_add(8)),
            tsf: u64_at(p, rate.saturating_add(12)).unwrap_or_default(),
            frame,
        })
    }
}
