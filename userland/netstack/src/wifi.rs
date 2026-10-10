//! YOGA WIFI HACK (measurement image only, never lands): the Intel AX200
//! (8086:2723, "cc-a0"), from its firmware to a passive scan.
//!
//! Written against OpenBSD's ISC-licensed `if_iwx` (the 22000-family path):
//! the PCIe transport with a context-info firmware load, one command queue
//! and one receive queue, the NVM read, the init sequence `iwx_init_hw` sends,
//! a PHY and a MAC context, and `SCAN_REQ_UMAC` in its v14 layout (the one
//! `iwx` sends to firmware that reports v15). Every step is logged with the
//! prefix `wifi:`; an SSID is never logged, only answered to `wifi scan`.
//!
//! The kernel hands the function over unremapped and untranslated, armed with
//! MSI: a device address is a physical address. Nothing here waits on the
//! interrupt; every wait polls the receive ring against a deadline, and the
//! claim is drained each pass so the poller does not spin on it.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use toyos::say;
use toyos::shm::SharedMemory;
use toyos::volatile::Window;
use toyos::{DmaRegion, PciDev};

/// Where the image puts `assets/firmware/iwlwifi-cc-a0-77.ucode`.
pub const FIRMWARE: &str = "/system/share/firmware/iwlwifi-cc-a0-77.ucode";

/// `wifi scan`'s request; its one payload byte is a [`Mode`].
pub const MSG_WIFI_SCAN: u32 = u32::from_le_bytes(*b"wifi");

/// How the request wants the device before it scans.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Scan on the firmware as it is, bringing it up first if it is down.
    Scan,
    /// Bring the device up again from reset, with PHY and MAC contexts.
    Reset,
    /// Bring the device up again from reset without contexts, then scan.
    Bare,
}

impl Mode {
    pub fn from_byte(byte: Option<u8>) -> Self {
        match byte {
            Some(1) => Self::Reset,
            Some(2) => Self::Bare,
            _ => Self::Scan,
        }
    }
}

mod csr {
    pub const HW_IF_CONFIG: usize = 0x000;
    pub const INT_COALESCING: usize = 0x004;
    pub const INT: usize = 0x008;
    pub const INT_MASK: usize = 0x00c;
    pub const FH_INT_STATUS: usize = 0x010;
    pub const RESET: usize = 0x020;
    pub const GP_CNTRL: usize = 0x024;
    pub const HW_REV: usize = 0x028;
    pub const GIO: usize = 0x03c;
    pub const CTXT_INFO_BA: usize = 0x040;
    pub const UCODE_DRV_GP1_CLR: usize = 0x05c;
    pub const MBOX_SET: usize = 0x088;
    pub const HW_RF_ID: usize = 0x09c;
    pub const MAC_SHADOW_REG_CTRL: usize = 0x0a8;
    pub const LTR_LONG_VAL_AD: usize = 0x0d4;
    pub const GIO_CHICKEN: usize = 0x100;
    pub const DBG_HPET_MEM: usize = 0x240;
    pub const DBG_LINK_PWR_MGMT: usize = 0x250;
    pub const MAC_ADDR_BASE: usize = 0x380;

    pub const HBUS_TARG_MEM_RADDR: usize = 0x40c;
    pub const HBUS_TARG_MEM_RDAT: usize = 0x41c;
    pub const HBUS_TARG_PRPH_WADDR: usize = 0x444;
    pub const HBUS_TARG_PRPH_RADDR: usize = 0x448;
    pub const HBUS_TARG_PRPH_WDAT: usize = 0x44c;
    pub const HBUS_TARG_PRPH_RDAT: usize = 0x450;
    pub const HBUS_TARG_WRPTR: usize = 0x460;
    pub const RFH_Q0_FRBDCB_WIDX_TRG: usize = 0x1c80;
    pub const MSIX_FH_CAUSES: usize = 0x2800;
    pub const MSIX_FH_MASK: usize = 0x2804;
    pub const MSIX_HW_CAUSES: usize = 0x2808;
    pub const MSIX_HW_MASK: usize = 0x280c;

    pub const HW_IF_NIC_READY: u32 = 0x0040_0000;
    pub const HW_IF_PREPARE: u32 = 0x0800_0000;
    pub const HW_IF_HAP_WAKE_L1A: u32 = 0x0008_0000;
    pub const MBOX_OS_ALIVE: u32 = 0x20;
    pub const LINK_PWR_MGMT_DISABLED: u32 = 0x8000_0000;
    pub const RESET_SW: u32 = 0x80;

    pub const GP_MAC_CLOCK_READY: u32 = 0x1;
    pub const GP_INIT_DONE: u32 = 0x4;
    pub const GP_MAC_ACCESS_REQ: u32 = 0x8;
    pub const GP_GOING_TO_SLEEP: u32 = 0x10;
    pub const GP_RFKILL_WAKE_L1A_EN: u32 = 0x0400_0000;
    pub const GP_HW_RF_KILL_SW: u32 = 0x0800_0000;

    pub const INT_FH_RX: u32 = 1 << 31;
    pub const INT_HW_ERR: u32 = 1 << 29;
    pub const INT_RX_PERIODIC: u32 = 1 << 28;
    pub const INT_FH_TX: u32 = 1 << 27;
    pub const INT_SW_ERR: u32 = 1 << 25;
    pub const INT_RF_KILL: u32 = 1 << 7;
    pub const INT_SW_RX: u32 = 1 << 3;
    pub const INT_WAKEUP: u32 = 1 << 1;
    pub const INT_ALIVE: u32 = 1 << 0;
    pub const INT_INI_SET: u32 = INT_FH_RX
        | INT_HW_ERR
        | INT_FH_TX
        | INT_SW_ERR
        | INT_RF_KILL
        | INT_SW_RX
        | INT_WAKEUP
        | INT_ALIVE
        | INT_RX_PERIODIC;
    pub const FH_INT_RX_MASK: u32 = (1 << 30) | (1 << 17) | (1 << 16);

    pub const GP1_RFKILL: u32 = 0x2;
    pub const GP1_CMD_BLOCKED: u32 = 0x4;
}

mod prph {
    pub const UREG_CHICK: u32 = 0xa05c00;
    pub const UREG_CHICK_MSI_ENABLE: u32 = 1 << 24;
    pub const UREG_UCODE_LOAD_STATUS: u32 = 0xa05c40;
    pub const UREG_CPU_INIT_RUN: u32 = 0xa05c44;
    pub const HPM_DEBUG: u32 = 0xa03440;
    pub const PERSISTENCE_BIT: u32 = 1 << 12;
    pub const PREG_PRPH_WPROT_22000: u32 = 0xa04d00;
    pub const PREG_WFPM_ACCESS: u32 = 1 << 12;
    pub const RFH_RXF_DMA_CFG: u32 = 0xa09820;
    pub const RFH_GEN_STATUS: u32 = 0xa09808;
    pub const RXF_DMA_IDLE: u32 = 1 << 31;
}

/// Command groups and opcodes, as `if_iwxreg.h` names them.
mod cmd {
    pub const LEGACY: u8 = 0x0;
    pub const LONG: u8 = 0x1;
    pub const SYSTEM: u8 = 0x2;
    pub const PHY_OPS: u8 = 0x4;
    pub const REGULATORY: u8 = 0xc;

    pub const ALIVE: u8 = 0x1;
    pub const REPLY_ERROR: u8 = 0x2;
    pub const INIT_COMPLETE_NOTIF: u8 = 0x4;
    pub const PHY_CONTEXT: u8 = 0x8;
    pub const SCAN_CFG: u8 = 0xc;
    pub const SCAN_REQ_UMAC: u8 = 0xd;
    pub const SCAN_COMPLETE_UMAC: u8 = 0xf;
    pub const MAC_CONTEXT: u8 = 0x28;
    pub const POWER_TABLE: u8 = 0x77;
    pub const TX_ANT_CONFIGURATION: u8 = 0x98;
    pub const BT_CONFIG: u8 = 0x9b;
    pub const SCAN_ITERATION_COMPLETE_UMAC: u8 = 0xb5;
    pub const REPLY_RX_MPDU: u8 = 0xc1;
    pub const MCC_UPDATE: u8 = 0xc8;
    pub const MCC_CHUB_UPDATE: u8 = 0xc9;
    pub const BEACON_FILTERING: u8 = 0xd2;
    pub const LTR_CONFIG: u8 = 0xee;

    pub const SOC_CONFIGURATION: u8 = 0x01;
    pub const INIT_EXTENDED_CFG: u8 = 0x03;
    pub const TEMP_REPORTING_THRESHOLDS: u8 = 0x04;
    pub const NVM_ACCESS_COMPLETE: u8 = 0x00;
    pub const NVM_GET_INFO: u8 = 0x02;

    pub const FAILED_MSK: u8 = 0x40;
}

const RX_RING: usize = 512;
const RBUF: usize = 4096;
const TX_RING: usize = 256;
const TFD_BYTES: usize = 256;
const CMD_BUFS: usize = 32;
const FIRST_TB: usize = 20;
const CTXT_INFO_BYTES: usize = 1792;
const ALIVE_STATUS_OK: u16 = 0xcafe;
const CPU1_CPU2_SEPARATOR: u32 = 0xffff_cccc;
const PAGING_SEPARATOR: u32 = 0xaaaa_bbbb;
const CHANNELS_8000: [u8; 51] = [
    1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 36, 40, 44, 48, 52, 56, 60, 64, 68, 72, 76, 80, 84, 88,
    92, 96, 100, 104, 108, 112, 116, 120, 124, 128, 132, 136, 140, 144, 149, 153, 157, 161, 165, 169, 173,
    177, 181,
];
const NUM_2GHZ: usize = 14;
const NVM_CHANNEL_VALID: u32 = 1 << 0;
const NVM_CHANNEL_ACTIVE: u32 = 1 << 3;
const SCAN_BOUND: Duration = Duration::from_secs(20);

fn now_ns() -> u64 {
    toyos_abi::clock::nanos_since_boot()
}

/// The delays `if_iwx` takes, which stand in for the hardware documentation
/// Intel does not publish.
fn delay_us(us: u64) {
    let end = now_ns() + us * 1000;
    while now_ns() < end {
        std::hint::spin_loop();
    }
}

/// Little-endian fields of something the device wrote; past its end reads 0.
fn le16(b: &[u8], at: usize) -> u16 {
    b.get(at..at + 2).map_or(0, |v| u16::from_le_bytes([v[0], v[1]]))
}

fn le32(b: &[u8], at: usize) -> u32 {
    b.get(at..at + 4).map_or(0, |v| u32::from_le_bytes([v[0], v[1], v[2], v[3]]))
}

// ---------------------------------------------------------------- firmware

struct Section {
    devoff: u32,
    at: usize,
    len: usize,
}

/// The firmware file, its TLVs read the way `iwx_read_firmware` reads them.
pub struct Firmware {
    raw: Vec<u8>,
    sections: Vec<Section>,
    lmac: usize,
    umac: usize,
    paging: usize,
    version: String,
    phy_config: u32,
    calib: (u32, u32),
    api: [u32; 4],
    capa: [u32; 4],
    cmd_versions: Vec<[u8; 4]>,
    n_scan_channels: u32,
    unknown_tlvs: usize,
}

impl Firmware {
    pub fn read() -> Result<Self, String> {
        let raw = std::fs::read(FIRMWARE).map_err(|e| format!("reading {FIRMWARE}: {e}"))?;
        Self::parse(raw)
    }

    fn parse(raw: Vec<u8>) -> Result<Self, String> {
        if raw.len() < 88 || le32(&raw, 0) != 0 || le32(&raw, 4) != 0x0a4c_5749 {
            return Err("not a TLV firmware image".into());
        }
        let ver = le32(&raw, 72);
        let mut fw = Self {
            raw: Vec::new(),
            sections: Vec::new(),
            lmac: 0,
            umac: 0,
            paging: 0,
            version: format!("{}.{}.{}", (ver >> 24) & 0xff, (ver >> 8) & 0xff, ver & 0xff),
            phy_config: 0,
            calib: (0, 0),
            api: [0; 4],
            capa: [0; 4],
            cmd_versions: Vec::new(),
            n_scan_channels: 40,
            unknown_tlvs: 0,
        };
        let mut at = 88;
        while at + 8 <= raw.len() {
            let kind = le32(&raw, at);
            let len = le32(&raw, at + 4) as usize;
            let data = at + 8;
            if data + len > raw.len() {
                return Err(format!("TLV {kind:#x} at {at:#x} runs {len} bytes past the file"));
            }
            let v = &raw[data..data + len];
            match kind {
                19 => {
                    if len < 4 {
                        return Err("a SEC_RT shorter than its offset".into());
                    }
                    fw.sections.push(Section { devoff: le32(v, 0), at: data + 4, len: len - 4 });
                }
                22 if len == 12 && le32(v, 0) == 0 => fw.calib = (le32(v, 4), le32(v, 8)),
                23 if len == 4 => fw.phy_config = le32(v, 0),
                29 if len == 8 => {
                    let idx = le32(v, 0) as usize;
                    if idx < 4 {
                        fw.api[idx] = le32(v, 4);
                    }
                }
                30 if len == 8 => {
                    let idx = le32(v, 0) as usize;
                    if idx < 4 {
                        fw.capa[idx] = le32(v, 4);
                    }
                }
                31 if len == 4 => fw.n_scan_channels = le32(v, 0),
                36 if len == 12 => {
                    fw.version = format!("{}.{:08x}.{}", le32(v, 0), le32(v, 4), le32(v, 8));
                }
                48 => fw.cmd_versions = v.chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]).collect(),
                _ => fw.unknown_tlvs += 1,
            }
            at = data + ((len + 3) & !3);
        }
        let count = |sections: &[Section], from: usize| {
            sections[from.min(sections.len())..]
                .iter()
                .take_while(|s| s.devoff != CPU1_CPU2_SEPARATOR && s.devoff != PAGING_SEPARATOR)
                .count()
        };
        fw.lmac = count(&fw.sections, 0);
        fw.umac = count(&fw.sections, fw.lmac + 1);
        fw.paging = count(&fw.sections, fw.lmac + fw.umac + 2);
        if fw.lmac == 0 || fw.umac == 0 || fw.lmac > 64 || fw.umac > 64 || fw.paging > 64 {
            return Err(format!("sections lmac {} umac {} paging {}", fw.lmac, fw.umac, fw.paging));
        }
        fw.raw = raw;
        Ok(fw)
    }

    fn api(&self, bit: usize) -> bool {
        self.api[bit / 32] >> (bit % 32) & 1 == 1
    }

    fn capa(&self, bit: usize) -> bool {
        self.capa[bit / 32] >> (bit % 32) & 1 == 1
    }

    /// `(cmd_ver, notif_ver)` the firmware declares for `grp`/`op`.
    fn cmd_version(&self, grp: u8, op: u8) -> Option<(u8, u8)> {
        self.cmd_versions.iter().find(|c| c[1] == grp && c[0] == op).map(|c| (c[2], c[3]))
    }

    /// The SEC_RT sections in the context info's three lists: LMAC, UMAC and
    /// paging, each section's index into [`Self::sections`].
    fn image(&self) -> [Vec<usize>; 3] {
        [
            (0..self.lmac).collect(),
            (self.lmac + 1..self.lmac + 1 + self.umac).collect(),
            (self.lmac + self.umac + 2..self.lmac + self.umac + 2 + self.paging).collect(),
        ]
    }

    pub fn summary(&self) -> String {
        let bytes: usize = self.sections.iter().map(|s| s.len).sum();
        format!(
            "fw {} ({} bytes), {} SEC_RT ({} lmac, {} umac, {} paging; {} bytes), phy_config {:#x}, \
             {} cmd versions, scan v{}, {} scan channels, LAR {}, api {:08x}.{:08x}.{:08x} capa \
             {:08x}.{:08x}.{:08x}.{:08x}, {} other TLVs",
            self.version,
            self.raw.len(),
            self.sections.len(),
            self.lmac,
            self.umac,
            self.paging,
            bytes,
            self.phy_config,
            self.cmd_versions.len(),
            self.cmd_version(cmd::LONG, cmd::SCAN_REQ_UMAC).map_or(0, |v| v.0),
            self.n_scan_channels,
            self.capa(1),
            self.api[0],
            self.api[1],
            self.api[2],
            self.capa[0],
            self.capa[1],
            self.capa[2],
            self.capa[3],
            self.unknown_tlvs,
        )
    }
}

// ------------------------------------------------------------------- scan

/// One network a scan heard.
#[derive(Clone)]
struct Bss {
    ssid: Vec<u8>,
    bssid: [u8; 6],
    channel: u8,
    rssi: i32,
    security: &'static str,
    heard: u32,
}

/// What a beacon or probe response says about its network, from its
/// information elements.
fn parse_mgmt(frame: &[u8], desc_channel: u8, rssi: i32) -> Option<Bss> {
    if frame.len() < 36 {
        return None;
    }
    let fc0 = frame[0];
    if fc0 & 0x0c != 0 {
        return None;
    }
    let subtype = fc0 & 0xf0;
    if subtype != 0x80 && subtype != 0x50 {
        return None;
    }
    let bssid: [u8; 6] = frame[16..22].try_into().unwrap();
    let capability = le16(frame, 34);
    let mut ssid = Vec::new();
    let mut ds = None;
    let mut ht = None;
    let mut rsn: Option<&[u8]> = None;
    let mut wpa1 = false;
    let mut at = 36;
    while at + 2 <= frame.len() {
        let id = frame[at];
        let len = frame[at + 1] as usize;
        let Some(body) = frame.get(at + 2..at + 2 + len) else { break };
        match id {
            0 => ssid = body.to_vec(),
            3 if len >= 1 => ds = Some(body[0]),
            48 => rsn = Some(body),
            61 if len >= 1 => ht = Some(body[0]),
            221 if len >= 4 && body[..4] == [0x00, 0x50, 0xf2, 0x01] => wpa1 = true,
            _ => {}
        }
        at += 2 + len;
    }
    let security = match rsn.map(akms) {
        Some(a) if a.sae && a.psk => "WPA2/WPA3",
        Some(a) if a.sae => "WPA3",
        Some(a) if a.owe => "OWE",
        Some(a) if a.eap => "WPA2-EAP",
        Some(a) if a.psk => "WPA2",
        Some(_) => "RSN?",
        None if wpa1 => "WPA",
        None if capability & 0x10 != 0 => "WEP",
        None => "open",
    };
    Some(Bss {
        ssid,
        bssid,
        channel: ds.or(ht).unwrap_or(desc_channel),
        rssi,
        security,
        heard: 1,
    })
}

#[derive(Default)]
struct Akms {
    psk: bool,
    sae: bool,
    eap: bool,
    owe: bool,
}

/// The AKM suites an RSN element offers (IEEE 802.11-2020 9.4.2.24).
fn akms(rsn: &[u8]) -> Akms {
    let mut out = Akms::default();
    // version 2, group cipher 4, pairwise count 2 + 4n, AKM count 2 + 4m
    let Some(pairwise) = rsn.get(6..8).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize) else {
        return out;
    };
    let at = 8 + 4 * pairwise;
    let Some(n) = rsn.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize) else {
        return out;
    };
    for i in 0..n {
        let Some(suite) = rsn.get(at + 2 + 4 * i..at + 6 + 4 * i) else { break };
        if suite[..3] != [0x00, 0x0f, 0xac] {
            continue;
        }
        match suite[3] {
            1 | 5 | 11 | 12 | 13 => out.eap = true,
            2 | 6 => out.psk = true,
            8 | 9 | 24 | 25 => out.sae = true,
            18 => out.owe = true,
            _ => {}
        }
    }
    out
}

/// An SSID as text: printable ASCII as it is, everything else escaped.
fn ssid_text(ssid: &[u8]) -> String {
    if ssid.is_empty() || ssid.iter().all(|b| *b == 0) {
        return "<hidden>".into();
    }
    match std::str::from_utf8(ssid) {
        Ok(s) if !s.chars().any(char::is_control) => s.to_string(),
        _ => ssid
            .iter()
            .map(|b| if b.is_ascii_graphic() || *b == b' ' { (*b as char).to_string() } else { format!("\\x{b:02x}") })
            .collect(),
    }
}

struct Scan {
    began: Instant,
    frames: u32,
    found: BTreeMap<[u8; 6], Bss>,
}

// ----------------------------------------------------------------- device

/// Whether the function's PCIe Device Control 2 has LTR enabled, read the
/// way `iwx_apm_config` reads it; logged with the capability's offset.
fn pcie_ltr_enabled(claim: &PciDev) -> bool {
    use toyos_abi::syscall::RegWidth;
    let read = |at: u32, width| claim.config_read(at, width).unwrap_or(0);
    let mut at = read(0x34, RegWidth::U8) & 0xfc;
    for _ in 0..48 {
        if at == 0 {
            break;
        }
        if read(at, RegWidth::U8) == 0x10 {
            let dcsr2 = read(at + 0x28, RegWidth::U16);
            let lctl = read(at + 0x10, RegWidth::U16);
            say!("wifi: PCIe cap at {at:#x}: link control {lctl:#06x}, device control 2 {dcsr2:#06x}");
            return dcsr2 & (1 << 10) != 0;
        }
        at = read(at + 1, RegWidth::U8) & 0xfc;
    }
    say!("wifi: no PCIe capability found; LTR taken as off");
    false
}

/// Where each structure sits in the one grant.
struct Layout {
    ctxt: usize,
    rx_free: usize,
    rx_used: usize,
    rx_stat: usize,
    tfd: usize,
    cmd_bufs: usize,
    rx_bufs: usize,
    /// Each SEC_RT section's offset, by index into `Firmware::sections`.
    sections: Vec<usize>,
    bytes: usize,
}

impl Layout {
    fn of(fw: &Firmware) -> Self {
        let mut at = 0usize;
        let mut take = |bytes: usize, align: usize| {
            at = (at + align - 1) & !(align - 1);
            let here = at;
            at += bytes;
            here
        };
        let ctxt = take(CTXT_INFO_BYTES, 4096);
        let rx_free = take(RX_RING * 8, 4096);
        let rx_used = take(RX_RING * 4, 4096);
        let rx_stat = take(16, 4096);
        let tfd = take(TX_RING * TFD_BYTES, 4096);
        let cmd_bufs = take(CMD_BUFS * 4096, 4096);
        let rx_bufs = take(RX_RING * RBUF, 4096);
        let sections = fw.sections.iter().map(|s| take(s.len.max(1), 4096)).collect();
        let bytes = (take(0, 4096) + (2 << 20) - 1) & !((2 << 20) - 1);
        Self { ctxt, rx_free, rx_used, rx_stat, tfd, cmd_bufs, rx_bufs, sections, bytes }
    }
}

/// What the NVM said, summarised.
#[derive(Default, Clone)]
struct Nvm {
    version: u16,
    sku: u32,
    tx_chains: u32,
    rx_chains: u32,
    lar: bool,
    profile: Vec<u32>,
}

enum Phase {
    Down(String),
    Up,
    Scanning(Scan),
}

struct Inner {
    bar: Window,
    dma: Window,
    dma_base: u64,
    fw: Firmware,
    layout: Layout,
    hw_rev: u32,
    mac: Option<[u8; 6]>,
    nvm: Option<Nvm>,
    /// Channel number and whether the firmware may only listen there.
    channels: Vec<(u8, bool)>,
    contexts: bool,
    /// Whether the firmware was ever started, so a bring-up stops it first.
    kicked: bool,
    msix_seen: bool,
    /// PCIe Device Control 2 says LTR is enabled, which is when iwx sends LTR_CONFIG.
    ltr: bool,
    phase: Phase,
    nic_locks: u32,
    rx_cur: usize,
    tx_cur: usize,
    alive: Option<bool>,
    init_complete: bool,
    resp: Option<(u8, u8, Vec<u8>)>,
    err_tables: (u32, u32),
    unhandled: BTreeMap<u16, u32>,
    finished: Option<String>,
    last: Option<String>,
    irqs: u64,
    scans: u32,
}

/// The AX200, brought up or with the reason it is not.
pub struct Wifi {
    inner: RefCell<Inner>,
    claim: Rc<PciDev>,
    _bar: SharedMemory,
    _region: DmaRegion,
}

impl Wifi {
    /// Take the claim, map BAR 0 and one grant, and bring the device up; a
    /// device that does not come up is kept with the reason, for `wifi scan`.
    pub fn open(claim: PciDev) -> Result<Self, String> {
        let claim = Rc::new(claim);
        let info = claim.describe().map_err(|e| format!("describe: {e:?}"))?;
        say!(
            "wifi: PCI {:02x}:{:02x}.{} bar bytes {:?}",
            info.bus,
            info.dev,
            info.func,
            info.bar_bytes
        );
        let (index, bytes) = info
            .bar_bytes
            .iter()
            .enumerate()
            .find(|(_, b)| **b >= 0x2000)
            .map(|(i, b)| (i as u32, *b))
            .ok_or("no BAR of 8 KiB or more")?;
        let bar_map = claim.map_bar(index, bytes).map_err(|e| format!("map_bar {index}: {e:?}"))?;
        // SAFETY: `map_bar` answered `bytes` bytes of live mapping, and
        // `bar_map` lives in the returned `Wifi` beside the window.
        let bar = unsafe { Window::new(bar_map.as_ptr(), bytes as usize) };

        let fw = Firmware::read()?;
        say!("wifi: {}", fw.summary());
        let layout = Layout::of(&fw);
        let region = claim
            .dma_alloc(layout.bytes as u64)
            .map_err(|e| format!("dma_alloc {} bytes: {e:?}", layout.bytes))?;
        // SAFETY: `dma_alloc` answered `layout.bytes` bytes of live mapping,
        // and `region` lives in the returned `Wifi` beside the window.
        let dma = unsafe { Window::new(region.memory.as_ptr(), layout.bytes) };
        dma.zero();
        say!(
            "wifi: BAR {index} {bytes:#x} bytes; grant {} bytes at device {:#x}; ctxt {:#x} rx_free {:#x} \
             rx_used {:#x} rx_stat {:#x} tfd {:#x} cmd {:#x} rx_bufs {:#x}",
            layout.bytes,
            region.device_addr,
            layout.ctxt,
            layout.rx_free,
            layout.rx_used,
            layout.rx_stat,
            layout.tfd,
            layout.cmd_bufs,
            layout.rx_bufs,
        );
        let mut inner = Inner {
            bar,
            dma,
            dma_base: region.device_addr,
            fw,
            layout,
            hw_rev: 0,
            mac: None,
            nvm: None,
            channels: Vec::new(),
            contexts: true,
            kicked: false,
            msix_seen: false,
            ltr: false,
            phase: Phase::Down("not brought up".into()),
            nic_locks: 0,
            rx_cur: 0,
            tx_cur: 0,
            alive: None,
            init_complete: false,
            resp: None,
            err_tables: (0, 0),
            unhandled: BTreeMap::new(),
            finished: None,
            last: None,
            irqs: 0,
            scans: 0,
        };
        inner.ltr = pcie_ltr_enabled(&claim);
        let raw_rev = inner.r32(csr::HW_REV);
        inner.hw_rev = (raw_rev & 0xfff0) | ((raw_rev & 3) << 2);
        say!(
            "wifi: HW_REV {raw_rev:#010x} (rev {:#x}) HW_RF_ID {:#010x} GP_CNTRL {:#010x} HW_IF_CONFIG {:#010x}",
            inner.hw_rev,
            inner.r32(csr::HW_RF_ID),
            inner.r32(csr::GP_CNTRL),
            inner.r32(csr::HW_IF_CONFIG),
        );
        inner.bring_up(true);
        Ok(Self { inner: RefCell::new(inner), claim, _bar: bar_map, _region: region })
    }

    pub fn claim(&self) -> &PciDev {
        &self.claim
    }

    /// The NVM's address, or a locally administered stand-in while the device
    /// is down: the node needs one either way, and no frame leaves this card.
    pub fn mac(&self) -> [u8; 6] {
        self.inner.borrow().mac.unwrap_or([0x02, 0x00, 0x00, 0x00, 0x27, 0x23])
    }

    /// Drain the claim and serve the device: a pass's first call.
    pub fn pass(&self) {
        let mut inner = self.inner.borrow_mut();
        while let Ok(record) = self.claim.irq() {
            inner.irqs += u64::from(record.count);
        }
        if matches!(inner.phase, Phase::Down(_)) {
            return;
        }
        if let Err(why) = inner.service() {
            inner.die(why);
        }
        inner.check_scan_deadline();
    }

    /// Start a scan, bringing the device up first where `mode` or its state
    /// asks; the answer arrives through [`Self::take_finished`].
    pub fn request(&self, mode: Mode) {
        let mut guard = self.inner.borrow_mut();
        let inner = &mut *guard;
        say!("wifi: scan requested ({mode:?})");
        if matches!(inner.phase, Phase::Scanning(_)) {
            say!("wifi: a scan is running already; the request waits for it");
            return;
        }
        match mode {
            Mode::Reset => inner.bring_up(true),
            Mode::Bare => inner.bring_up(false),
            Mode::Scan if matches!(inner.phase, Phase::Down(_)) => {
                let contexts = inner.contexts;
                inner.bring_up(contexts);
            }
            Mode::Scan => {}
        }
        if let Phase::Down(why) = &inner.phase {
            inner.finished = Some(format!("wifi: the AX200 is down: {why}\n"));
            return;
        }
        if let Err(why) = inner.start_scan() {
            inner.die(why.clone());
            inner.finished = Some(format!("wifi: the scan did not start: {why}\n"));
        }
    }

    /// The text of a scan that has ended since the last call.
    pub fn take_finished(&self) -> Option<String> {
        self.inner.borrow_mut().finished.take()
    }

    /// How long until a pass has to look at the ring while a scan runs.
    pub fn due_in(&self) -> Option<u64> {
        matches!(self.inner.borrow().phase, Phase::Scanning(_)).then_some(20_000_000)
    }

    pub fn inspect(&self, snap: &mut toyos_inspect::Snapshot) {
        let inner = self.inner.borrow();
        snap.put("driver", "ax200");
        snap.put("link.state", "down");
        snap.put(
            "wifi.phase",
            match &inner.phase {
                Phase::Down(why) => format!("down: {why}"),
                Phase::Up => "up".to_string(),
                Phase::Scanning(_) => "scanning".to_string(),
            },
        );
        snap.put("wifi.irqs", inner.irqs);
        snap.put("wifi.scans", u64::from(inner.scans));
    }
}

impl Inner {
    // ----------------------------------------------------- register access

    fn r32(&self, at: usize) -> u32 {
        self.bar.read::<u32>(at)
    }

    fn w32(&self, at: usize, value: u32) {
        self.bar.write::<u32>(at, value);
    }

    fn w8(&self, at: usize, value: u8) {
        self.bar.write::<u8>(at, value);
    }

    fn set_bits(&self, at: usize, bits: u32) {
        self.w32(at, self.r32(at) | bits);
    }

    fn clr_bits(&self, at: usize, bits: u32) {
        self.w32(at, self.r32(at) & !bits);
    }

    fn poll_bit(&self, at: usize, bits: u32, mask: u32, timeout_us: u64) -> bool {
        let end = now_ns() + timeout_us * 1000;
        loop {
            if self.r32(at) & mask == bits & mask {
                return true;
            }
            if now_ns() > end {
                return false;
            }
            delay_us(10);
        }
    }

    fn nic_lock(&mut self) -> bool {
        if self.nic_locks > 0 {
            self.nic_locks += 1;
            return true;
        }
        self.set_bits(csr::GP_CNTRL, csr::GP_MAC_ACCESS_REQ);
        delay_us(2);
        if self.poll_bit(
            csr::GP_CNTRL,
            csr::GP_MAC_CLOCK_READY,
            csr::GP_MAC_CLOCK_READY | csr::GP_GOING_TO_SLEEP,
            150_000,
        ) {
            self.nic_locks += 1;
            true
        } else {
            say!("wifi: acquiring the NIC failed: GP_CNTRL {:#010x}", self.r32(csr::GP_CNTRL));
            false
        }
    }

    fn nic_unlock(&mut self) {
        if self.nic_locks > 0 {
            self.nic_locks -= 1;
            if self.nic_locks == 0 {
                self.clr_bits(csr::GP_CNTRL, csr::GP_MAC_ACCESS_REQ);
            }
        }
    }

    fn read_prph(&self, addr: u32) -> u32 {
        self.w32(csr::HBUS_TARG_PRPH_RADDR, (addr & 0x000f_ffff) | (3 << 24));
        std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
        self.r32(csr::HBUS_TARG_PRPH_RDAT)
    }

    fn write_prph(&self, addr: u32, value: u32) {
        self.w32(csr::HBUS_TARG_PRPH_WADDR, (addr & 0x000f_ffff) | (3 << 24));
        std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
        self.w32(csr::HBUS_TARG_PRPH_WDAT, value);
    }

    fn read_mem(&mut self, addr: u32, dwords: usize) -> Option<Vec<u32>> {
        if !self.nic_lock() {
            return None;
        }
        self.w32(csr::HBUS_TARG_MEM_RADDR, addr);
        let out = (0..dwords).map(|_| self.r32(csr::HBUS_TARG_MEM_RDAT)).collect();
        self.nic_unlock();
        Some(out)
    }

    // ------------------------------------------------------------ DMA access

    fn dev(&self, at: usize) -> u64 {
        self.dma_base + at as u64
    }

    fn d16(&self, at: usize, value: u16) {
        self.dma.write::<u16>(at, value);
    }

    fn d32(&self, at: usize, value: u32) {
        self.dma.write::<u32>(at, value);
    }

    fn d64(&self, at: usize, value: u64) {
        self.dma.write::<u64>(at, value);
    }

    /// A u64 at an offset only two-byte aligned, as the TFD's buffers are.
    fn d64_unaligned(&self, at: usize, value: u64) {
        for i in 0..4 {
            self.d16(at + 2 * i, (value >> (16 * i)) as u16);
        }
    }

    fn dma_put(&self, at: usize, data: &[u8]) {
        let whole = data.len() & !7;
        if whole > 0 {
            self.dma.copy_in(at, &data[..whole]);
        }
        for (i, byte) in data[whole..].iter().enumerate() {
            self.dma.write::<u8>(at + whole + i, *byte);
        }
    }

    fn dma_zero(&self, at: usize, bytes: usize) {
        for i in (0..bytes).step_by(8) {
            self.d64(at + i, 0);
        }
    }

    // --------------------------------------------------------------- bring-up

    fn die(&mut self, why: String) {
        say!("wifi: DOWN: {why}");
        if matches!(self.phase, Phase::Scanning(_)) {
            self.finished = Some(format!("wifi: the scan ended with the device: {why}\n"));
        }
        self.phase = Phase::Down(why);
    }

    fn bring_up(&mut self, contexts: bool) {
        self.contexts = contexts;
        let began = Instant::now();
        match self.try_bring_up(contexts) {
            Ok(()) => {
                say!("wifi: up in {} ms (contexts {contexts})", began.elapsed().as_millis());
                self.phase = Phase::Up;
            }
            Err(why) => {
                self.die(why);
                self.dump_state();
            }
        }
    }

    fn try_bring_up(&mut self, contexts: bool) -> Result<(), String> {
        self.nic_locks = 0;
        self.alive = None;
        self.init_complete = false;
        self.resp = None;
        self.w32(csr::INT_MASK, 0);
        self.w32(csr::INT, !0);
        self.w32(csr::FH_INT_STATUS, !0);
        if self.kicked {
            self.stop_device();
        }
        self.start_hw()?;
        self.start_fw()?;
        self.wait_alive()?;
        self.w32(csr::INT_MASK, csr::INT_INI_SET);
        self.init_nvm()?;
        self.init_hw()?;
        if contexts {
            self.add_contexts()?;
        } else {
            say!("wifi: contexts skipped (bare)");
        }
        Ok(())
    }

    fn stop_device(&mut self) {
        say!("wifi: stopping the device before bringing it up again");
        self.w32(csr::INT_MASK, 0);
        self.w32(csr::INT, !0);
        self.w32(csr::FH_INT_STATUS, !0);
        if self.nic_lock() {
            self.write_prph(prph::RFH_RXF_DMA_CFG, 0);
            for _ in 0..1000 {
                if self.read_prph(prph::RFH_GEN_STATUS) & prph::RXF_DMA_IDLE != 0 {
                    break;
                }
                delay_us(10);
            }
            self.nic_unlock();
        }
        self.clr_bits(csr::GP_CNTRL, csr::GP_MAC_ACCESS_REQ);
        self.nic_locks = 0;
        // iwx_apm_stop
        self.set_bits(csr::DBG_LINK_PWR_MGMT, csr::LINK_PWR_MGMT_DISABLED);
        self.set_bits(csr::HW_IF_CONFIG, csr::HW_IF_PREPARE | 0x1000_0000);
        delay_us(1000);
        self.clr_bits(csr::DBG_LINK_PWR_MGMT, csr::LINK_PWR_MGMT_DISABLED);
        delay_us(5000);
        self.set_bits(csr::RESET, 0x200);
        if !self.poll_bit(csr::RESET, 0x100, 0x100, 100) {
            say!("wifi: timeout waiting for the bus master to stop");
        }
        self.clr_bits(csr::GP_CNTRL, csr::GP_INIT_DONE);
        self.sw_reset();
        self.w32(csr::INT_MASK, 0);
        self.w32(csr::INT, !0);
    }

    fn set_hw_ready(&self) -> bool {
        self.set_bits(csr::HW_IF_CONFIG, csr::HW_IF_NIC_READY);
        let ready = self.poll_bit(csr::HW_IF_CONFIG, csr::HW_IF_NIC_READY, csr::HW_IF_NIC_READY, 50);
        if ready {
            self.set_bits(csr::MBOX_SET, csr::MBOX_OS_ALIVE);
        }
        ready
    }

    fn prepare_card_hw(&self) -> Result<(), String> {
        if self.set_hw_ready() {
            return Ok(());
        }
        self.set_bits(csr::DBG_LINK_PWR_MGMT, csr::LINK_PWR_MGMT_DISABLED);
        delay_us(1000);
        let mut t = 0;
        for _ in 0..10 {
            self.set_bits(csr::HW_IF_CONFIG, csr::HW_IF_PREPARE);
            loop {
                if self.set_hw_ready() {
                    return Ok(());
                }
                delay_us(200);
                t += 200;
                if t >= 150_000 {
                    break;
                }
            }
            delay_us(25_000);
        }
        Err(format!("the card never said NIC_READY: HW_IF_CONFIG {:#010x}", self.r32(csr::HW_IF_CONFIG)))
    }

    fn sw_reset(&self) {
        self.set_bits(csr::RESET, csr::RESET_SW);
        delay_us(5000);
    }

    fn apm_init(&self) -> Result<(), String> {
        self.set_bits(csr::GIO_CHICKEN, 0x0080_0000);
        self.set_bits(csr::DBG_HPET_MEM, 0xffff_0000);
        self.set_bits(csr::HW_IF_CONFIG, csr::HW_IF_HAP_WAKE_L1A);
        self.set_bits(csr::GIO, 0x2);
        self.set_bits(csr::GP_CNTRL, csr::GP_INIT_DONE);
        if !self.poll_bit(csr::GP_CNTRL, csr::GP_MAC_CLOCK_READY, csr::GP_MAC_CLOCK_READY, 25_000) {
            return Err(format!("no clock stabilisation: GP_CNTRL {:#010x}", self.r32(csr::GP_CNTRL)));
        }
        Ok(())
    }

    fn start_hw(&mut self) -> Result<(), String> {
        self.prepare_card_hw()?;
        say!("wifi: NIC ready, HW_IF_CONFIG {:#010x}", self.r32(csr::HW_IF_CONFIG));
        let hpm = self.read_prph(prph::HPM_DEBUG);
        if hpm != 0xa5a5_a5a0 && hpm & prph::PERSISTENCE_BIT != 0 {
            let wprot = self.read_prph(prph::PREG_PRPH_WPROT_22000);
            if wprot & prph::PREG_WFPM_ACCESS != 0 {
                return Err(format!("the persistence bit cannot be cleared: HPM_DEBUG {hpm:#x} WPROT {wprot:#x}"));
            }
            self.write_prph(prph::HPM_DEBUG, hpm & !prph::PERSISTENCE_BIT);
            say!("wifi: persistence bit cleared (HPM_DEBUG {hpm:#x})");
        }
        self.sw_reset();
        self.apm_init()?;
        if !self.nic_lock() {
            return Err("no NIC access to switch on MSI".into());
        }
        self.write_prph(prph::UREG_CHICK, prph::UREG_CHICK_MSI_ENABLE);
        self.nic_unlock();
        self.w32(csr::MSIX_FH_MASK, !0);
        self.w32(csr::MSIX_HW_MASK, !0);
        self.w32(csr::INT_MASK, csr::INT_RF_KILL);
        self.set_bits(csr::GP_CNTRL, csr::GP_RFKILL_WAKE_L1A_EN);
        let gp = self.r32(csr::GP_CNTRL);
        say!(
            "wifi: started: GP_CNTRL {gp:#010x}, radio {}",
            if gp & csr::GP_HW_RF_KILL_SW == 0 { "KILLED by the hardware switch" } else { "enabled" }
        );
        Ok(())
    }

    fn reset_rings(&mut self) {
        let l = &self.layout;
        let (rx_free, rx_used, rx_stat, tfd, rx_bufs) = (l.rx_free, l.rx_used, l.rx_stat, l.tfd, l.rx_bufs);
        self.dma_zero(rx_stat, 16);
        self.dma_zero(rx_used, RX_RING * 4);
        self.dma_zero(tfd, TX_RING * TFD_BYTES);
        for i in 0..RX_RING {
            self.d64(rx_free + 8 * i, self.dev(rx_bufs + i * RBUF) | i as u64);
        }
        self.rx_cur = 0;
        self.tx_cur = 0;
    }

    fn start_fw(&mut self) -> Result<(), String> {
        self.w32(csr::INT, !0);
        self.w32(csr::INT_MASK, 0);
        self.w32(csr::INT, !0);
        self.w32(csr::FH_INT_STATUS, !0);
        self.w32(csr::UCODE_DRV_GP1_CLR, csr::GP1_RFKILL);
        self.w32(csr::UCODE_DRV_GP1_CLR, csr::GP1_CMD_BLOCKED);
        self.w32(csr::INT, !0);
        // iwx_nic_init
        self.apm_init()?;
        let phy = self.fw.phy_config;
        let (radio_type, radio_step, radio_dash) = (phy & 3, (phy >> 2) & 3, (phy >> 4) & 3);
        let reg = ((self.hw_rev >> 2) & 3) << 2
            | (self.hw_rev & 3)
            | radio_type << 10
            | radio_step << 14
            | radio_dash << 12;
        let mask = 0x3 | 0xc | 0xc000 | 0x3000 | 0xc00 | 0x200 | 0x100;
        let hw_if = (self.r32(csr::HW_IF_CONFIG) & !mask) | reg;
        self.w32(csr::HW_IF_CONFIG, hw_if);
        self.w8(csr::INT_COALESCING, 0x40);
        self.set_bits(csr::MAC_SHADOW_REG_CTRL, 0x800f_ffff);
        self.w32(csr::INT_MASK, csr::INT_ALIVE | csr::INT_FH_RX);

        self.reset_rings();
        self.write_context_info()?;
        Ok(())
    }

    fn write_context_info(&mut self) -> Result<(), String> {
        let c = self.layout.ctxt;
        self.dma_zero(c, CTXT_INFO_BYTES.next_multiple_of(8));
        self.d16(c, self.r32(csr::HW_REV) as u16);
        self.d16(c + 2, 0);
        self.d16(c + 4, (CTXT_INFO_BYTES / 4) as u16);
        // TFD format long, RBD ring of 2^9, 4 KiB receive buffers.
        let control = (1 << 8) | (9 << 4) | (4 << 9);
        self.d32(c + 8, control);
        self.d64(c + 24, self.dev(self.layout.rx_free));
        self.d64(c + 32, self.dev(self.layout.rx_used));
        self.d64(c + 40, self.dev(self.layout.rx_stat));
        self.d64(c + 48, self.dev(self.layout.tfd));
        self.dma.write::<u8>(c + 56, 5); // log2(256) - 3
        let [lmac, umac, paging] = self.fw.image();
        let mut copied = 0usize;
        for (list, base) in [(&umac, 192usize), (&lmac, 192 + 512), (&paging, 192 + 1024)] {
            for (slot, &index) in list.iter().enumerate() {
                let s = &self.fw.sections[index];
                let at = self.layout.sections[index];
                let data = &self.fw.raw[s.at..s.at + s.len];
                self.dma_put(at, data);
                copied += s.len;
                self.d64(c + base + 8 * slot, self.dev(at));
            }
        }
        say!(
            "wifi: context info at device {:#x}: control {control:#x}, {} lmac {} umac {} paging sections, {copied} bytes",
            self.dev(c),
            lmac.len(),
            umac.len(),
            paging.len(),
        );
        let addr = self.dev(c);
        self.w32(csr::CTXT_INFO_BA, addr as u32);
        self.w32(csr::CTXT_INFO_BA + 4, (addr >> 32) as u32);
        if !self.nic_lock() {
            return Err("no NIC access to start the firmware".into());
        }
        let ltr = 0x8000_0000 | ((2 << 24) & 0x1c00_0000) | ((250 << 16) & 0x03ff_0000) | 0x8000 | ((2 << 8) & 0x1c00) | 250;
        self.w32(csr::LTR_LONG_VAL_AD, ltr);
        self.write_prph(prph::UREG_CPU_INIT_RUN, 1);
        self.kicked = true;
        self.nic_unlock();
        say!("wifi: firmware self-load kicked (UREG_CPU_INIT_RUN)");
        Ok(())
    }

    fn wait_alive(&mut self) -> Result<(), String> {
        let began = Instant::now();
        while began.elapsed() < Duration::from_secs(2) {
            self.service()?;
            match self.alive {
                Some(true) => {
                    say!("wifi: firmware alive after {} ms", began.elapsed().as_millis());
                    return Ok(());
                }
                Some(false) => return Err("the firmware's alive said it is not OK".into()),
                None => delay_us(200),
            }
        }
        let load = if self.nic_lock() {
            let v = self.read_prph(prph::UREG_UCODE_LOAD_STATUS);
            self.nic_unlock();
            v
        } else {
            0xdead_dead
        };
        Err(format!(
            "no alive in 2 s: INT {:#010x} FH_INT {:#010x} GP_CNTRL {:#010x} UCODE_LOAD_STATUS {load:#010x} rx closed {}",
            self.r32(csr::INT),
            self.r32(csr::FH_INT_STATUS),
            self.r32(csr::GP_CNTRL),
            self.dma.read::<u16>(self.layout.rx_stat),
        ))
    }

    fn dump_state(&mut self) {
        say!(
            "wifi: state: INT {:#010x} INT_MASK {:#010x} FH_INT {:#010x} GP_CNTRL {:#010x} HW_IF_CONFIG {:#010x} \
             rx_cur {} rx closed {} tx_cur {}",
            self.r32(csr::INT),
            self.r32(csr::INT_MASK),
            self.r32(csr::FH_INT_STATUS),
            self.r32(csr::GP_CNTRL),
            self.r32(csr::HW_IF_CONFIG),
            self.rx_cur,
            self.dma.read::<u16>(self.layout.rx_stat),
            self.tx_cur,
        );
        self.dump_errors();
    }

    fn dump_errors(&mut self) {
        let (lmac, umac) = self.err_tables;
        for (name, base) in [("lmac", lmac), ("umac", umac)] {
            if base == 0 {
                continue;
            }
            match self.read_mem(base & !0xc000_0000, 24) {
                Some(words) => say!(
                    "wifi: {name} error table @{base:#x}: {}",
                    words.iter().map(|w| format!("{w:08x}")).collect::<Vec<_>>().join(" ")
                ),
                None => say!("wifi: {name} error table @{base:#x}: unreadable"),
            }
        }
    }

    // ------------------------------------------------------ interrupt and rx

    /// Acknowledge what the device raised and take every closed receive
    /// buffer; an error the firmware or hardware raised is the answer.
    fn service(&mut self) -> Result<(), String> {
        let r1 = self.r32(csr::INT);
        if r1 == 0xffff_ffff {
            return Err("the device reads all ones: it is gone from the bus".into());
        }
        if r1 & 0xffff_fff0 == 0xa5a5_a5a0 {
            return Ok(());
        }
        if r1 != 0 {
            self.w32(csr::INT, r1);
        }
        // Where the MSI chicken bit did not take, the causes are MSI-X's.
        let (fh, hw) = (self.r32(csr::MSIX_FH_CAUSES), self.r32(csr::MSIX_HW_CAUSES));
        if fh | hw != 0 && fh != 0xffff_ffff {
            self.w32(csr::MSIX_FH_CAUSES, fh);
            self.w32(csr::MSIX_HW_CAUSES, hw);
            if !self.msix_seen {
                self.msix_seen = true;
                say!("wifi: MSI-X causes raised: fh {fh:#010x} hw {hw:#010x}");
            }
        }
        let r1 = r1
            | if hw & 1 != 0 { csr::INT_ALIVE } else { 0 }
            | if hw & (1 << 25) != 0 { csr::INT_SW_ERR } else { 0 }
            | if hw & (1 << 29) != 0 { csr::INT_HW_ERR } else { 0 };
        if r1 & csr::INT_ALIVE != 0 {
            // The firmware has configured the RFH: hand it the ring again.
            for i in 0..RX_RING {
                self.d64(self.layout.rx_free + 8 * i, self.dev(self.layout.rx_bufs + i * RBUF) | i as u64);
            }
            self.w32(csr::RFH_Q0_FRBDCB_WIDX_TRG, 8);
            say!("wifi: ALIVE interrupt (INT {r1:#010x}); 8 receive buffers handed over");
        }
        if r1 & (csr::INT_FH_RX | csr::INT_SW_RX) != 0 {
            self.w32(csr::FH_INT_STATUS, csr::FH_INT_RX_MASK);
        }
        if r1 & csr::INT_RF_KILL != 0 {
            say!("wifi: RF kill changed: GP_CNTRL {:#010x}", self.r32(csr::GP_CNTRL));
        }
        self.drain_rx();
        if r1 & csr::INT_SW_ERR != 0 {
            self.dump_errors();
            return Err(format!("firmware error (INT {r1:#010x})"));
        }
        if r1 & csr::INT_HW_ERR != 0 {
            return Err(format!("hardware error (INT {r1:#010x}, FH_INT {:#010x})", self.r32(csr::FH_INT_STATUS)));
        }
        Ok(())
    }

    fn drain_rx(&mut self) {
        let hw = (self.dma.read::<u16>(self.layout.rx_stat) as usize & 0xfff) & (RX_RING - 1);
        if hw == self.rx_cur {
            return;
        }
        let mut buf = vec![0u8; RBUF];
        while self.rx_cur != hw {
            let at = self.layout.rx_bufs + self.rx_cur * RBUF;
            self.dma.sub(at, RBUF).copy_out(0, &mut buf);
            self.rx_buffer(&buf);
            self.rx_cur = (self.rx_cur + 1) % RX_RING;
        }
        let back = if hw == 0 { RX_RING - 1 } else { hw - 1 };
        self.w32(csr::RFH_Q0_FRBDCB_WIDX_TRG, (back & !7) as u32);
    }

    fn rx_buffer(&mut self, buf: &[u8]) {
        let mut offset = 0;
        while offset + 8 < RBUF {
            let len_n_flags = le32(buf, offset);
            let (code, flags, idx, qid) = (buf[offset + 4], buf[offset + 5], buf[offset + 6], buf[offset + 7]);
            if (qid & 0x7f == 0 && idx == 0 && code == 0) || len_n_flags == 0x5555_0000 {
                break;
            }
            let pkt_len = (len_n_flags & 0x3fff) as usize;
            let len = 4 + pkt_len;
            if pkt_len < 4 || len > RBUF - offset {
                break;
            }
            let payload = &buf[offset + 8..offset + 4 + pkt_len];
            self.packet(flags, code, idx, qid, payload);
            offset += (len + 63) & !63;
        }
    }

    fn packet(&mut self, group: u8, code: u8, idx: u8, qid: u8, payload: &[u8]) {
        let legacy = group == cmd::LEGACY || group == cmd::LONG;
        let notification = qid & 0x80 != 0;
        match (legacy, code) {
            (true, cmd::ALIVE) if notification || self.alive.is_none() => {
                let status = le16(payload, 0);
                let ok = status == ALIVE_STATUS_OK;
                if payload.len() >= 112 {
                    self.err_tables = (le32(payload, 20), le32(payload, 108));
                    say!(
                        "wifi: ALIVE status {status:#x} ({} bytes): lmac {}.{} umac {}.{} lmac err {:#x} umac err {:#x}",
                        payload.len(),
                        le32(payload, 4),
                        le32(payload, 8),
                        le32(payload, 100),
                        le32(payload, 104),
                        self.err_tables.0,
                        self.err_tables.1,
                    );
                } else {
                    say!("wifi: ALIVE status {status:#x} ({} bytes, short)", payload.len());
                }
                self.alive = Some(ok);
                return;
            }
            (true, cmd::INIT_COMPLETE_NOTIF) if notification => {
                say!("wifi: INIT_COMPLETE");
                self.init_complete = true;
                return;
            }
            (true, cmd::REPLY_RX_MPDU) => {
                self.rx_mpdu(payload);
                return;
            }
            (true, cmd::SCAN_ITERATION_COMPLETE_UMAC) if notification => {
                say!(
                    "wifi: scan iteration complete: {} channels, status {}",
                    payload.get(4).copied().unwrap_or(0),
                    payload.get(5).copied().unwrap_or(0)
                );
                self.end_scan("iteration complete");
                return;
            }
            (true, cmd::SCAN_COMPLETE_UMAC) if notification => {
                say!(
                    "wifi: scan complete: status {} ebs {}",
                    payload.get(6).copied().unwrap_or(0),
                    payload.get(7).copied().unwrap_or(0)
                );
                self.end_scan("complete");
                return;
            }
            (true, cmd::REPLY_ERROR) => {
                say!(
                    "wifi: firmware REPLY_ERROR type {:#x} cmd {:#x} seq {:#x}",
                    le32(payload, 0),
                    payload.get(4).copied().unwrap_or(0),
                    if payload.len() >= 8 { le16(payload, 6) } else { 0 }
                );
                return;
            }
            (true, cmd::MCC_CHUB_UPDATE) if notification => {
                say!("wifi: MCC_CHUB_UPDATE mcc {:#06x} source {}", le16(payload, 0), payload.get(2).copied().unwrap_or(0));
                return;
            }
            _ => {}
        }
        if !notification && qid == 0 {
            self.resp = Some((idx, group, payload.to_vec()));
            return;
        }
        let key = (u16::from(group) << 8) | u16::from(code);
        let seen = self.unhandled.entry(key).or_insert(0);
        *seen += 1;
        if *seen == 1 {
            say!("wifi: notification {group:#x}/{code:#x} ({} bytes, qid {qid:#x}) not handled", payload.len());
        }
    }

    // --------------------------------------------------------------- commands

    /// Send one command on the command queue and wait for its answer: the
    /// response's header flags and payload.
    fn send(&mut self, grp: u8, op: u8, payload: &[u8]) -> Result<(u8, Vec<u8>), String> {
        let wire_grp = if grp == cmd::LEGACY { cmd::LONG } else { grp };
        let idx = self.tx_cur;
        let buf = self.layout.cmd_bufs + (idx % CMD_BUFS) * 4096;
        let total = 8 + payload.len();
        if total > 4096 {
            return Err(format!("command {grp:#x}/{op:#x} is {total} bytes"));
        }
        let mut bytes = Vec::with_capacity(total);
        bytes.extend_from_slice(&[op, wire_grp, idx as u8, 0]);
        bytes.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        bytes.extend_from_slice(&[0, 0]);
        bytes.extend_from_slice(payload);
        self.dma_put(buf, &bytes);
        let tfd = self.layout.tfd + idx * TFD_BYTES;
        self.dma_zero(tfd, TFD_BYTES);
        let first = total.min(FIRST_TB);
        self.d16(tfd + 2, first as u16);
        self.d64_unaligned(tfd + 4, self.dev(buf));
        if total > FIRST_TB {
            self.d16(tfd + 12, (total - FIRST_TB) as u16);
            self.d64_unaligned(tfd + 14, self.dev(buf + FIRST_TB));
            self.d16(tfd, 2);
        } else {
            self.d16(tfd, 1);
        }
        std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
        self.tx_cur = (self.tx_cur + 1) % TX_RING;
        self.resp = None;
        self.w32(csr::HBUS_TARG_WRPTR, self.tx_cur as u32);
        let began = Instant::now();
        loop {
            self.service()?;
            if let Some((ridx, flags, data)) = self.resp.take() {
                if usize::from(ridx) == idx {
                    let status = if data.len() >= 4 { format!("{:#x}", le32(&data, 0)) } else { "-".into() };
                    say!(
                        "wifi: cmd {grp:#x}/{op:#x} ({} bytes) idx {idx}: flags {flags:#x}, {} byte answer, word0 {status}, {} us",
                        payload.len(),
                        data.len(),
                        began.elapsed().as_micros()
                    );
                    if flags & cmd::FAILED_MSK != 0 {
                        return Err(format!("command {grp:#x}/{op:#x} failed (flags {flags:#x})"));
                    }
                    return Ok((flags, data));
                }
                say!("wifi: an answer for idx {ridx} while waiting for {idx}");
            }
            if began.elapsed() > Duration::from_secs(2) {
                return Err(format!(
                    "command {grp:#x}/{op:#x} idx {idx} unanswered in 2 s (INT {:#010x}, rx closed {})",
                    self.r32(csr::INT),
                    self.dma.read::<u16>(self.layout.rx_stat)
                ));
            }
            delay_us(50);
        }
    }

    fn init_nvm(&mut self) -> Result<(), String> {
        self.send(cmd::SYSTEM, cmd::INIT_EXTENDED_CFG, &(1u32 << 1).to_le_bytes())?;
        self.send(cmd::REGULATORY, cmd::NVM_ACCESS_COMPLETE, &0u32.to_le_bytes())?;
        let began = Instant::now();
        while !self.init_complete {
            if began.elapsed() > Duration::from_secs(2) {
                return Err("no INIT_COMPLETE in 2 s".into());
            }
            self.service()?;
            delay_us(200);
        }
        let (_, rsp) = self.send(cmd::REGULATORY, cmd::NVM_GET_INFO, &0u32.to_le_bytes())?;
        if rsp.len() != 28 + 4 * 110 {
            return Err(format!("NVM_GET_INFO answered {} bytes, not 468", rsp.len()));
        }
        let nvm = Nvm {
            version: le16(&rsp, 4),
            sku: le32(&rsp, 8),
            tx_chains: le32(&rsp, 12),
            rx_chains: le32(&rsp, 16),
            lar: le32(&rsp, 20) != 0 && self.fw.capa(1),
            profile: (0..CHANNELS_8000.len()).map(|i| le32(&rsp, 28 + 4 * i)).collect(),
        };
        if !self.nic_lock() {
            return Err("no NIC access to read the MAC address".into());
        }
        let flip = |a0: u32, a1: u32| {
            let (a, b) = (a0.to_le_bytes(), a1.to_le_bytes());
            [a[3], a[2], a[1], a[0], b[1], b[0]]
        };
        let strap = flip(self.r32(csr::MAC_ADDR_BASE + 8), self.r32(csr::MAC_ADDR_BASE + 12));
        let otp = flip(self.r32(csr::MAC_ADDR_BASE), self.r32(csr::MAC_ADDR_BASE + 4));
        self.nic_unlock();
        let valid = |m: &[u8; 6]| {
            *m != [0x02, 0xcc, 0xaa, 0xff, 0xee, 0x00] && *m != [0xff; 6] && *m != [0; 6] && m[0] & 1 == 0
        };
        let mac = if valid(&strap) { strap } else { otp };
        if !valid(&mac) {
            return Err("no valid MAC address in the strap or the OTP".into());
        }
        self.mac = Some(mac);
        let valid_channels = nvm.profile.iter().filter(|f| **f & NVM_CHANNEL_VALID != 0).count();
        say!(
            "wifi: NVM version {:#x}, sku {:#x} (2.4 {} 5.2 {} n {} ac {}), tx chains {:#x} rx chains {:#x}, LAR {}, \
             {valid_channels} valid channels, MAC {}",
            nvm.version,
            nvm.sku,
            nvm.sku & 1 != 0,
            nvm.sku & 2 != 0,
            nvm.sku & 4 != 0,
            nvm.sku & 8 != 0,
            nvm.tx_chains,
            nvm.rx_chains,
            nvm.lar,
            mac.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":"),
        );
        self.set_channels(&nvm.profile, nvm.sku & 2 != 0);
        self.nvm = Some(nvm);
        Ok(())
    }

    fn set_channels(&mut self, profile: &[u32], five: bool) {
        self.channels = CHANNELS_8000
            .iter()
            .zip(profile)
            .enumerate()
            .filter(|(i, (_, flags))| **flags & NVM_CHANNEL_VALID != 0 && (*i < NUM_2GHZ || five))
            .map(|(_, (ch, flags))| (*ch, *flags & NVM_CHANNEL_ACTIVE == 0))
            .take(self.fw.n_scan_channels.min(67) as usize)
            .collect();
        say!(
            "wifi: {} channels to scan: {}",
            self.channels.len(),
            self.channels.iter().map(|(c, p)| format!("{c}{}", if *p { "p" } else { "" })).collect::<Vec<_>>().join(" ")
        );
    }

    fn valid_tx_ant(&self) -> u32 {
        let fw = (self.fw.phy_config >> 16) & 0xf;
        match self.nvm.as_ref().map(|n| n.tx_chains) {
            Some(n) if n != 0 => fw & n,
            _ => fw,
        }
    }

    fn valid_rx_ant(&self) -> u32 {
        let fw = (self.fw.phy_config >> 20) & 0xf;
        match self.nvm.as_ref().map(|n| n.rx_chains) {
            Some(n) if n != 0 => fw & n,
            _ => fw,
        }
    }

    fn init_hw(&mut self) -> Result<(), String> {
        if !self.nic_lock() {
            return Err("no NIC access for the init commands".into());
        }
        let result = self.init_hw_locked();
        self.nic_unlock();
        result
    }

    fn init_hw_locked(&mut self) -> Result<(), String> {
        self.send(cmd::LEGACY, cmd::TX_ANT_CONFIGURATION, &self.valid_tx_ant().to_le_bytes())?;
        let mut bt = Vec::new();
        bt.extend_from_slice(&3u32.to_le_bytes());
        bt.extend_from_slice(&0u32.to_le_bytes());
        self.send(cmd::LEGACY, cmd::BT_CONFIG, &bt)?;
        let mut soc = Vec::new();
        soc.extend_from_slice(&1u32.to_le_bytes()); // discrete
        soc.extend_from_slice(&0u32.to_le_bytes());
        self.send(cmd::SYSTEM, cmd::SOC_CONFIGURATION, &soc)?;
        if self.fw.capa(12) {
            self.send(5, 0x00, &0u32.to_le_bytes())?;
        }
        if self.ltr {
            let mut ltr = vec![0u8; 32];
            ltr[..4].copy_from_slice(&1u32.to_le_bytes());
            self.send(cmd::LEGACY, cmd::LTR_CONFIG, &ltr)?;
        }
        if self.fw.capa(74) {
            self.send(cmd::PHY_OPS, cmd::TEMP_REPORTING_THRESHOLDS, &[0u8; 20])?;
        }
        self.send(cmd::LEGACY, cmd::POWER_TABLE, &[1, 0, 0, 0])?;
        if self.nvm.as_ref().is_some_and(|n| n.lar) {
            self.mcc_update()?;
        }
        let mut scan_cfg = vec![0u8; 12];
        let scan_cfg_ver = self.fw.cmd_version(cmd::LONG, cmd::SCAN_CFG).map_or(0, |v| v.0);
        if scan_cfg_ver < 5 {
            scan_cfg[2] = 0xff;
        }
        scan_cfg[4..8].copy_from_slice(&self.valid_tx_ant().to_le_bytes());
        scan_cfg[8..12].copy_from_slice(&self.valid_rx_ant().to_le_bytes());
        self.send(cmd::LONG, cmd::SCAN_CFG, &scan_cfg)?;
        self.send(cmd::LEGACY, cmd::BEACON_FILTERING, &[0u8; 60])?;
        Ok(())
    }

    fn mcc_update(&mut self) -> Result<(), String> {
        let mut mcc = vec![0u8; 32];
        mcc[..2].copy_from_slice(&(u16::from(b'Z') << 8 | u16::from(b'Z')).to_le_bytes());
        mcc[2] = if self.fw.api(9) || self.fw.capa(29) { 0x10 } else { 0 };
        let (_, rsp) = self.send(cmd::LEGACY, cmd::MCC_UPDATE, &mcc)?;
        if rsp.len() < 20 {
            say!("wifi: MCC_UPDATE answered {} bytes; keeping the NVM's channels", rsp.len());
            return Ok(());
        }
        let n = le32(&rsp, 16) as usize;
        say!(
            "wifi: MCC_UPDATE status {:#x} mcc {:#06x} cap {:#x} source {} {} channels",
            le32(&rsp, 0),
            le16(&rsp, 4),
            le16(&rsp, 6),
            rsp[12],
            n
        );
        if rsp.len() == 20 + 4 * n && n >= NUM_2GHZ {
            let profile: Vec<u32> = (0..n.min(CHANNELS_8000.len())).map(|i| le32(&rsp, 20 + 4 * i)).collect();
            let five = self.nvm.as_ref().is_some_and(|nvm| nvm.sku & 2 != 0);
            self.set_channels(&profile, five);
        }
        Ok(())
    }

    fn add_contexts(&mut self) -> Result<(), String> {
        let rx = self.valid_rx_ant();
        let mut phy = Vec::with_capacity(32);
        phy.extend_from_slice(&0u32.to_le_bytes()); // id 0, color 0
        phy.extend_from_slice(&1u32.to_le_bytes()); // add
        phy.extend_from_slice(&1u32.to_le_bytes()); // channel 1
        phy.extend_from_slice(&[1, 0, 0, 0]); // 2.4 GHz, 20 MHz, control below
        phy.extend_from_slice(&0u32.to_le_bytes()); // LMAC 2.4
        phy.extend_from_slice(&(rx << 1 | 1 << 10 | 1 << 12).to_le_bytes());
        phy.extend_from_slice(&0u32.to_le_bytes());
        phy.extend_from_slice(&0u32.to_le_bytes());
        self.send(cmd::LEGACY, cmd::PHY_CONTEXT, &phy)?;

        let mac = self.mac.unwrap_or([0; 6]);
        let mut m = vec![0u8; 148];
        m[4..8].copy_from_slice(&1u32.to_le_bytes()); // add
        m[8..12].copy_from_slice(&5u32.to_le_bytes()); // BSS station
        m[16..22].copy_from_slice(&mac);
        m[32..36].copy_from_slice(&0x0fu32.to_le_bytes());
        m[36..40].copy_from_slice(&0x15u32.to_le_bytes());
        m[52..56].copy_from_slice(&((1u32 << 2) | (1 << 6)).to_le_bytes()); // group, beacons
        // EDCA defaults (ecwmin, ecwmax, aifsn, txop/32us) for BE BK VI VO,
        // in the GEN2 FIFOs BE=2 BK=1 VI=3 VO=4.
        for (fifo, ecwmin, ecwmax, aifsn, txop) in [(2usize, 4u32, 10u32, 3u8, 0u16), (1, 4, 10, 7, 0), (3, 3, 4, 2, 94), (4, 2, 3, 2, 47)] {
            let at = 60 + 8 * fifo;
            m[at..at + 2].copy_from_slice(&(((1u32 << ecwmin) - 1) as u16).to_le_bytes());
            m[at + 2..at + 4].copy_from_slice(&(((1u32 << ecwmax) - 1) as u16).to_le_bytes());
            m[at + 4] = aifsn;
            m[at + 5] = 1 << fifo;
            m[at + 6..at + 8].copy_from_slice(&(txop * 32).to_le_bytes());
        }
        m[116..120].copy_from_slice(&100u32.to_le_bytes()); // beacon interval
        m[124..128].copy_from_slice(&100u32.to_le_bytes()); // dtim interval
        m[132..136].copy_from_slice(&10u32.to_le_bytes()); // listen interval
        self.send(cmd::LEGACY, cmd::MAC_CONTEXT, &m)?;
        Ok(())
    }

    // ------------------------------------------------------------------ scan

    fn start_scan(&mut self) -> Result<(), String> {
        let mac = self.mac.unwrap_or([0; 6]);
        let mut c = vec![0u8; 1940];
        c[4..8].copy_from_slice(&6u32.to_le_bytes()); // ooc priority EXT_6
        // general params v10
        let flags: u16 = (1 << 1) | (1 << 2) | (1 << 7) | (1 << 11); // pass all, notify iteration, adaptive dwell, passive
        c[8..10].copy_from_slice(&flags.to_le_bytes());
        c[12] = 10;
        c[13] = 10;
        c[14] = 2;
        c[15] = 8;
        c[16] = 10;
        c[18..20].copy_from_slice(&300u16.to_le_bytes());
        c[36..40].copy_from_slice(&6u32.to_le_bytes());
        c[40] = 110;
        c[41] = 110;
        // channel params v6
        c[44] = 1 << 5;
        c[45] = self.channels.len() as u8;
        c[46] = 10;
        c[47] = 2;
        for (i, (channel, _)) in self.channels.iter().enumerate() {
            let at = 48 + 8 * i;
            c[at + 4] = *channel;
            c[at + 5] = if *channel <= 14 { 1 } else { 0 };
            c[at + 6] = 1;
        }
        // periodic params: one iteration
        c[586] = 1;
        // probe request (unused by a passive scan, filled as iwx fills it)
        let preq = 596;
        let buf = preq + 20;
        let mut frame = vec![0x40, 0x00, 0, 0];
        frame.extend_from_slice(&[0xff; 6]);
        frame.extend_from_slice(&mac);
        frame.extend_from_slice(&[0xff; 6]);
        frame.extend_from_slice(&[0, 0, 0, 0]);
        let header = frame.len() as u16;
        let rates_2g = frame.len() as u16;
        frame.extend_from_slice(&[1, 8, 2, 4, 11, 22, 12, 18, 24, 36, 50, 4, 48, 72, 96, 108]);
        if self.fw.capa(9) {
            frame.extend_from_slice(&[3, 1, 0]);
        }
        let rates_2g_len = frame.len() as u16 - rates_2g;
        let rates_5g = frame.len() as u16;
        frame.extend_from_slice(&[1, 8, 12, 18, 24, 36, 48, 72, 96, 108]);
        let rates_5g_len = frame.len() as u16 - rates_5g;
        let common = frame.len() as u16;
        c[preq..preq + 2].copy_from_slice(&0u16.to_le_bytes());
        c[preq + 2..preq + 4].copy_from_slice(&header.to_le_bytes());
        c[preq + 4..preq + 6].copy_from_slice(&rates_2g.to_le_bytes());
        c[preq + 6..preq + 8].copy_from_slice(&rates_2g_len.to_le_bytes());
        c[preq + 8..preq + 10].copy_from_slice(&rates_5g.to_le_bytes());
        c[preq + 10..preq + 12].copy_from_slice(&rates_5g_len.to_le_bytes());
        c[preq + 16..preq + 18].copy_from_slice(&common.to_le_bytes());
        c[buf..buf + frame.len()].copy_from_slice(&frame);

        let version = self.fw.cmd_version(cmd::LONG, cmd::SCAN_REQ_UMAC);
        say!(
            "wifi: SCAN_REQ_UMAC (fw declares {version:?}; v14 layout, 1940 bytes), passive, {} channels",
            self.channels.len()
        );
        self.send(cmd::LONG, cmd::SCAN_REQ_UMAC, &c)?;
        self.scans += 1;
        self.phase = Phase::Scanning(Scan { began: Instant::now(), frames: 0, found: BTreeMap::new() });
        Ok(())
    }

    fn rx_mpdu(&mut self, payload: &[u8]) {
        let Phase::Scanning(scan) = &mut self.phase else { return };
        const DESC: usize = 48;
        if payload.len() < DESC {
            return;
        }
        let status = le32(payload, 12);
        if status & 0x3 != 0x3 {
            return;
        }
        let len = le16(payload, 0) as usize;
        let Some(frame) = payload.get(DESC..DESC + len) else { return };
        let energy = |e: u8| if e == 0 { -256 } else { -i32::from(e) };
        let rssi = energy(payload[32]).max(energy(payload[33]));
        let channel = payload[34];
        let frame: Vec<u8> = if payload[3] & 0x20 != 0 && frame.len() > 26 {
            frame[..24].iter().chain(&frame[26..]).copied().collect()
        } else {
            frame.to_vec()
        };
        scan.frames += 1;
        if let Some(bss) = parse_mgmt(&frame, channel, rssi) {
            scan.found
                .entry(bss.bssid)
                .and_modify(|seen| {
                    seen.heard += 1;
                    if bss.rssi > seen.rssi {
                        seen.rssi = bss.rssi;
                    }
                    if seen.ssid.is_empty() && !bss.ssid.is_empty() {
                        seen.ssid = bss.ssid.clone();
                    }
                })
                .or_insert(bss);
        }
    }

    fn check_scan_deadline(&mut self) {
        if let Phase::Scanning(scan) = &self.phase {
            if scan.began.elapsed() > SCAN_BOUND {
                say!("wifi: no scan-complete notification in {} s; answering with what was heard", SCAN_BOUND.as_secs());
                self.end_scan("deadline");
            }
        }
    }

    fn end_scan(&mut self, why: &str) {
        let Phase::Scanning(scan) = std::mem::replace(&mut self.phase, Phase::Up) else { return };
        let mut found: Vec<Bss> = scan.found.into_values().collect();
        found.sort_by(|a, b| b.rssi.cmp(&a.rssi));
        let ms = scan.began.elapsed().as_millis();
        say!("wifi: scan ended ({why}) after {ms} ms: {} frames, {} networks", scan.frames, found.len());
        let mut text = format!("{:<32}  {:<17}  {:>3}  {:>4}  {}\n", "SSID", "BSSID", "CH", "RSSI", "SECURITY");
        for bss in &found {
            let line = format!(
                "{:<32}  {}  {:>3}  {:>4}  {}\n",
                ssid_text(&bss.ssid),
                bss.bssid.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":"),
                bss.channel,
                bss.rssi,
                bss.security
            );
            if text.len() + line.len() > 7900 {
                break;
            }
            text.push_str(&line);
        }
        text.push_str(&format!("{} networks, {} frames, {ms} ms ({why})\n", found.len(), scan.frames));
        self.last = Some(text.clone());
        self.finished = Some(text);
    }
}
