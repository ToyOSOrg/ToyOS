//! PSR1 on the eDP panel on transcoder A, by Vol 12 "Panel Self Refresh" and
//! Vol 2c SRD_CTL, VIDEO_DIP_CTL and PSR_MASK.
//!
//! [`read_sink`] reads the sink's capabilities and PSR status over AUX, and
//! [`prepare`] programs both sides, so both run while PSR is off and DC5
//! and DC6 are disallowed (Vol 12 "AUX Functional Sequence" step 2). [`Prepared::activate`]
//! then enables SRD. Transcoder A's link "only operate[s] in standby" during
//! SRD (Vol 2c SRD_CTL Link Ctrl), so the sink is told the main link stays
//! active. Once enabled, no AUX transaction is made.
//!
//! A present reaches the panel by a flip of the scan-out's own surface,
//! which is a frame update event that ends SRD ([`Psr::update`]).

use toyos_device_memory::Registers;

use crate::{aux, regs, Clock, Programmed, Refusal};

/// The receiver capability field, from the DPCD revision.
const RECEIVER: u32 = 0x000;
const PSR_SUPPORT: u32 = 0x070;
const EDP_REVISION: u32 = 0x700;
const PSR_EN_CFG: u32 = 0x170;
const SET_POWER: u32 = 0x600;
/// PSR_ERROR_STATUS, then PSR_ESI, PSR_STATUS and the sink's
/// synchronisation latency.
const PSR_ERROR_STATUS: u32 = 0x2006;

const PSR_ENABLE: u8 = 1 << 0;
const MAIN_LINK_ACTIVE: u8 = 1 << 1;
const SET_POWER_CAPABLE: u8 = 1 << 7;
const D0: u8 = 0x01;
const SINK_INTERNAL_ERROR: u8 = 7;

/// The fewest idle frames before entry this driver programs.
const MIN_IDLE_FRAMES: u32 = 6;

/// Vol 12 "AUX IO Power Enabling" step 3: "Timeout and fail after 10 us."
const AUX_POWER_NANOS: u64 = 10_000;

/// What the sink said of PSR, as read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sink {
    /// DPCD 00000h-0000Fh: the receiver capabilities, from the revision.
    pub receiver: [u8; 16],
    /// DPCD 00070h-00071h: the PSR version, and its capabilities.
    pub psr: [u8; 2],
    /// DPCD 00700h-00701h: the eDP revision, and general capability 1.
    pub edp: [u8; 2],
    /// DPCD 02006h-02009h: PSR error status, PSR ESI, PSR status and the
    /// synchronisation latency.
    pub status: [u8; 4],
}

impl Sink {
    /// The setup time the sink needs on the entry packet, in µs.
    pub fn setup_us(&self) -> Result<u32, Refusal> {
        match (self.psr[1] >> 1) & 0b111 {
            0b111 => Err(Refusal::SinkSetupReserved),
            field => Ok(330 - 55 * u32::from(field)),
        }
    }

    /// Whether the sink may be enabled for PSR1, as read.
    fn usable(&self) -> Result<(), Refusal> {
        if self.receiver[0] == 0 {
            return Err(Refusal::NoReceiver);
        }
        if self.psr[0] == 0 {
            return Err(Refusal::NoPsr);
        }
        if self.edp[1] & SET_POWER_CAPABLE == 0 {
            return Err(Refusal::NoSetPower);
        }
        self.setup_us()?;
        let [errors, _, status, _] = self.status;
        if errors & 0b111 != 0 || status & 0b111 == SINK_INTERNAL_ERROR {
            return Err(Refusal::SinkErrors { errors, status });
        }
        Ok(())
    }

    fn idle_frames(&self) -> u32 {
        (u32::from(self.status[3] & 0xF) + 1).clamp(MIN_IDLE_FRAMES, regs::SRD_IDLE_FRAMES)
    }
}

/// Both sides programmed, SRD not yet enabled.
#[derive(Debug)]
pub struct Prepared {
    pub sink: Sink,
    pub setup_lines: u64,
    pub vblank_lines: u64,
    plane: usize,
}

/// SRD enabled on transcoder A.
#[derive(Debug)]
pub struct Psr {
    prepared: Prepared,
}

/// The sink's capabilities and PSR status, read with PSR off; DC states the
/// firmware allowed are disallowed first. A refusal writes only those and
/// AUX IO power.
pub fn read_sink<R: Registers, C: Clock>(regs: &R, clock: &C) -> Result<Sink, Refusal> {
    if regs.read32(regs::SRD_CTL_A) & regs::SRD_ENABLE != 0 || regs.read32(regs::PSR2_CTL_A) & regs::PSR2_ENABLE != 0 {
        return Err(Refusal::PsrOn);
    }
    let dc = regs.read32(regs::DC_STATE_EN);
    if dc & regs::DYNAMIC_DC_STATE != 0 {
        regs.write32(regs::DC_STATE_EN, dc & !regs::DYNAMIC_DC_STATE);
    }
    aux_power(regs, clock)?;
    let mut sink = Sink { receiver: [0; 16], psr: [0; 2], edp: [0; 2], status: [0; 4] };
    aux::read(regs, clock, RECEIVER, &mut sink.receiver).map_err(Refusal::Aux)?;
    aux::read(regs, clock, PSR_SUPPORT, &mut sink.psr).map_err(Refusal::Aux)?;
    aux::read(regs, clock, EDP_REVISION, &mut sink.edp).map_err(Refusal::Aux)?;
    aux::read(regs, clock, PSR_ERROR_STATUS, &mut sink.status).map_err(Refusal::Aux)?;
    Ok(sink)
}

/// Both sides programmed for PSR1 on `programmed`'s scan-out, for `sink` as
/// [`read_sink`] read it. A refusal before the sink's configuration writes
/// nothing.
pub fn prepare<R: Registers, C: Clock>(regs: &R, clock: &C, programmed: &Programmed, sink: Sink) -> Result<Prepared, Refusal> {
    let scanout = &programmed.scanout;
    if regs.read32(regs::PSR_IIR_A) & regs::PSR_AUX_ERROR != 0 {
        return Err(Refusal::PsrAuxError);
    }
    let lines = |at| u64::from(regs.read32(at) & regs::LINES) + 1;
    let (vactive, vblank_start) = (lines(regs::TRANS_VTOTAL_A), lines(regs::TRANS_VBLANK_A));
    if vblank_start != vactive {
        return Err(Refusal::DelayedVblank { start: vblank_start, active: vactive });
    }
    if let Some(level) = (0..regs::LEVELS).find(|&l| regs.read32(regs::plane_wm(0, scanout.plane, l)) & regs::WM_ENABLE == 0) {
        return Err(Refusal::WatermarkLevelOff(level));
    }
    sink.usable()?;
    // Vol 2c SRD_CTL: "SRD must not be enabled when the PSR Setup time from
    // DPCD 00071h is greater than the time for vertical blank minus one line."
    let setup_us = u64::from(sink.setup_us()?);
    let setup_lines = (setup_us * programmed.pixel_rate_khz).div_ceil(1000 * scanout.htotal);
    let vblank_lines = scanout.vtotal - vactive;
    if setup_lines > vblank_lines - 1 {
        return Err(Refusal::SetupTime { lines: setup_lines, vblank: vblank_lines });
    }

    aux::write(regs, clock, PSR_EN_CFG, &[MAIN_LINK_ACTIVE]).map_err(Refusal::Aux)?;
    aux::write(regs, clock, PSR_EN_CFG, &[MAIN_LINK_ACTIVE | PSR_ENABLE]).map_err(Refusal::Aux)?;
    aux::write(regs, clock, SET_POWER, &[D0]).map_err(Refusal::Aux)?;

    // The VSC DIP enabled before PSR, with hardware sending PSR1's header and
    // data ("Recommended for use when PSR is enabled and software is not
    // using VSC for other types of data").
    let dip = regs.read32(regs::VIDEO_DIP_CTL_A);
    regs.write32(regs::VIDEO_DIP_CTL_A, dip & !regs::VSC_SELECT | regs::VSC_ENABLE);
    regs.write32(regs::PSR_MASK_A, regs.read32(regs::PSR_MASK_A) | regs::PSR_MASKED);
    regs.write32(regs::PSR_IMR_A, !(regs::PSR_AUX_ERROR | regs::PSR_EXIT));
    regs.write32(regs::PSR_IIR_A, regs.read32(regs::PSR_IIR_A));
    Ok(Prepared { sink, setup_lines, vblank_lines, plane: scanout.plane })
}

/// AUX A's IO power requested, and awaited (Vol 12 "AUX IO Power Enabling").
fn aux_power<R: Registers, C: Clock>(regs: &R, clock: &C) -> Result<(), Refusal> {
    let request = regs.read32(regs::PWR_WELL_CTL_AUX2);
    if request & regs::AUX_A_REQUEST == 0 {
        regs.write32(regs::PWR_WELL_CTL_AUX2, request | regs::AUX_A_REQUEST);
    }
    let deadline = clock.nanos() + AUX_POWER_NANOS;
    loop {
        let late = clock.nanos() > deadline;
        if regs.read32(regs::PWR_WELL_CTL_AUX2) & regs::AUX_A_STATE != 0 {
            return Ok(());
        }
        if late {
            return Err(Refusal::AuxPower);
        }
    }
}

impl Prepared {
    /// SRD enabled. TP1 and TP2 take 2.5 ms each where a wake trains the
    /// link, and TP4 is skipped.
    pub fn activate<R: Registers>(self, regs: &R) -> Psr {
        let kept = regs.read32(regs::SRD_CTL_A) & regs::SRD_CONTEXT_RESTORE;
        let ctl = kept
            | regs::SRD_ENABLE
            | regs::SRD_MAX_SLEEP
            | regs::SRD_TP2_TIME
            | regs::SRD_TP4_SKIP
            | regs::SRD_TP1_TIME
            | self.sink.idle_frames();
        regs.write32(regs::SRD_CTL_A, ctl);
        Psr { prepared: self }
    }
}

impl Psr {
    /// A present arrived: the scan-out's surface flipped to itself, which
    /// ends SRD. SRD_STATUS as it read before.
    pub fn update<R: Registers>(&self, regs: &R) -> u32 {
        let status = regs.read32(regs::SRD_STATUS_A);
        regs.write32(regs::PSR_IIR_A, regs::PSR_EXIT);
        let surface = regs::plane_surf(0, self.prepared.plane);
        regs.write32(surface, regs.read32(surface));
        status
    }

    /// Whether the source is out of SRD: IDLE, or waiting to send a frame
    /// before an entry.
    pub fn awake<R: Registers>(&self, regs: &R) -> bool {
        matches!(regs.read32(regs::SRD_STATUS_A) >> regs::SRD_STATE_SHIFT, 0b000 | 0b001)
    }

    /// SRD disabled, and its return to IDLE awaited (Vol 12 PSR1 disable
    /// sequence: "up to one full frame time (1/refresh rate), plus SRD exit
    /// training time (max of 6ms), plus SRD aux channel handshake (max of
    /// 1.5ms)"). Whether it got there, and the preparation back: the panel
    /// stays configured, so [`Prepared::activate`] enables SRD again.
    pub fn disable<R: Registers, C: Clock>(self, regs: &R, clock: &C, frame_nanos: u64) -> (bool, Prepared) {
        regs.write32(regs::SRD_CTL_A, regs.read32(regs::SRD_CTL_A) & !regs::SRD_ENABLE);
        let deadline = clock.nanos() + frame_nanos + 7_500_000;
        let idle = loop {
            let late = clock.nanos() > deadline;
            if regs.read32(regs::SRD_STATUS_A) >> regs::SRD_STATE_SHIFT == 0 {
                break true;
            }
            if late {
                break false;
            }
        };
        (idle, self.prepared)
    }

    /// A PSR AUX error since the last look: SRD disabled, and the refusal.
    pub fn fault<R: Registers>(&self, regs: &R) -> Option<Refusal> {
        if regs.read32(regs::PSR_IIR_A) & regs::PSR_AUX_ERROR == 0 {
            return None;
        }
        regs.write32(regs::SRD_CTL_A, regs.read32(regs::SRD_CTL_A) & !regs::SRD_ENABLE);
        Some(Refusal::PsrAuxError)
    }
}
