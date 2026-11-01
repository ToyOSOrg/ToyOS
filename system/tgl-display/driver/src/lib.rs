//! Every decision the Tiger Lake display driver makes, against
//! [`Registers`] and a [`Clock`], from Intel's published references:
//! IHD-OS-TGL-Vol 12-12.21 (Display Engine) and IHD-OS-TGL-Vol 2c-12.21
//! (registers). Each register and sequence is cited where it is used.
//!
//! **What it does today is program power on the firmware's surface**: the
//! mode, the plane and its buffer are the GOP's and stay so.
//! [`keep_memory_low_power`] reads the one plane the firmware scans out,
//! pcode's memory latencies and SAGV block time, computes watermark levels 0-7
//! by Vol 12's algorithm ([`watermark`]) and programs them, so the display lets
//! memory reach the latencies the levels name. [`allow_display_c6`] loads the
//! DMC's program ([`dmc`]), powers down the wells the scan-out does not use and
//! lets the engine enter DC5 and DC6. Anything it does not understand of the
//! firmware's state is refused by name. A refusal before a write leaves the
//! state as found, and no refusal allows DC6.

#![no_std]
#![forbid(unsafe_code)]

use core::fmt;

use toyos_device_memory::Registers;

pub mod aux;
pub mod dmc;
pub mod interrupt;
pub mod psr;
pub mod regs;
pub mod watermark;

#[cfg(test)]
mod dc_tests;
#[cfg(test)]
mod psr_tests;
#[cfg(test)]
mod model;
#[cfg(test)]
mod tests;

use watermark::{Latencies, Pipe, Plane, Tiling, Watermarks};

/// The machine's monotonic clock, and a pause that gives the processor away.
pub trait Clock {
    fn nanos(&self) -> u64;
    fn pause(&self, nanos: u64);
}

/// Frames the pixel rate is measured over: half a second at 60 Hz.
const MEASURED_FRAMES: u32 = 30;
/// The longest one frame may take before the pipe is called stopped: two
/// frames at 24 Hz. A hang's ceiling.
pub const FRAME_CEILING_NANOS: u64 = 84_000_000;
/// Between two reads of the frame counter.
const FRAME_POLL_NANOS: u64 = 50_000;
/// Vol 12 "Retrieve Memory Latency Data" and "SAGV Block Time": "Timeout after
/// 100 us and do not enable display planes."
const MAILBOX_NANOS: u64 = 100_000;
/// Vol 12 "Power Well" disable sequence: "timeout after 20 us".
const POWER_WELL_NANOS: u64 = 20_000;
/// The wells the eDP scan-out on pipe A does not use, highest first, the
/// order Vol 12 disables them in: PG5 holds pipe D, PG4 pipe C, PG3 pipe B,
/// external displays, VGA and audio. Pipe A, transcoder A with its PSR and DDI
/// A-C are PG1's (Vol 12 "Power Well", "PG1 contains the functions for pipe A
/// display on eDP").
const UNUSED_WELLS: [u32; 3] = [5, 4, 3];

/// Why this driver stopped short; [`keep_memory_low_power`],
/// [`allow_display_c6`] and [`interrupt::serve`] say how far each got.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The firmware's state is not one pipe A plane scanning out alone.
    NotOnePlane(&'static str),
    /// The plane's pixel format, PLANE_CTL 27:23, is not a packed RGB one.
    Format(u32),
    /// The plane's tiling, PLANE_CTL 12:10, is not one Vol 2c names.
    Tiling(u32),
    /// The plane is rotated 90 or 270 degrees (PLANE_CTL 1:0), or reads a
    /// compressed surface (bits 15 and 4), which the algorithm here does not
    /// size.
    Layout(u32),
    /// The frame counter did not advance within a frame's ceiling.
    PipeStopped,
    /// The mailbox stayed busy, or answered an error code, to `command`.
    Mailbox { command: u32, interface: u32 },
    /// The line time this driver computes disagrees with the firmware's by
    /// more than rounding: the pixel rate measured is not the mode's.
    LineTime { computed: u64, firmware: u32 },
    /// Level 0 does not fit the plane's allocation even without SAGV.
    LevelZero,
    /// The plane cannot tolerate SAGV, and masking QGV points takes the
    /// bandwidth calculation this driver does not yet make.
    SagvIntolerant,
    /// The PCI revision is not one Vol 4 gives a display stepping for.
    Revision(u8),
    /// The DMC package has no main program for the display stepping.
    NoProgram(dmc::Stepping),
    /// Transcoder A drives another DDI than A (TRANS_DDI_FUNC_CTL_A's DDI
    /// Select, 1h for A). Vol 12 "Sequence to Allow DC5 or DC6": "Ports A
    /// IO/PHY programming is preserved when DC6 is enabled. The programming
    /// for other combo PHY ports may not be."
    Port(u32),
    /// VGA display is enabled, which needs PG3.
    Vga,
    /// The CSR program storage did not hold a word the load wrote.
    NotStored { at: usize, wrote: u32, read: u32 },
    /// A power well stayed on after every request this driver can see was
    /// withdrawn.
    PowerWellHeld(u32),
    /// DISPLAY_INT_CTL named a pending category this driver enabled nothing
    /// in.
    Interrupts(u32),
    /// PSR or PSR2 was already enabled.
    PsrOn,
    /// PSR_IIR named an AUX error of PSR's own.
    PsrAuxError,
    /// Pipe A's vertical blank starts after vertical active. PSR there needs
    /// a latency-reporting change this driver does not make.
    DelayedVblank { start: u64, active: u64 },
    /// A watermark level of the plane is disabled. PSR there needs the same
    /// change.
    WatermarkLevelOff(usize),
    /// AUX A's IO power did not come on.
    AuxPower,
    /// An AUX transaction with the panel failed.
    Aux(aux::Fault),
    /// The panel's DPCD 00000h, its DPCD revision, reads 0: no receiver
    /// answered with its capabilities.
    NoReceiver,
    /// The panel's DPCD 00070h names no PSR version.
    NoPsr,
    /// The panel cannot be set to D0 over DPCD 00600h.
    NoSetPower,
    /// The panel's PSR setup time field is the reserved value.
    SinkSetupReserved,
    /// The panel reported PSR errors, or an internal error, before enable.
    SinkErrors { errors: u8, status: u8 },
    /// The panel's setup time, in lines, does not fit vertical blank less a
    /// line.
    SetupTime { lines: u64, vblank: u64 },
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotOnePlane(why) => write!(f, "the firmware's display is not one plane on pipe A: {why}"),
            Self::Format(format) => write!(f, "the plane's pixel format {format:#07b} is not packed RGB"),
            Self::Tiling(tiling) => write!(f, "the plane's tiling {tiling:#05b} is not one Vol 2c names"),
            Self::Layout(ctl) => write!(f, "the plane is rotated 90/270 or decompresses (PLANE_CTL {ctl:#010x})"),
            Self::PipeStopped => write!(f, "pipe A's frame counter did not advance within {} ms", FRAME_CEILING_NANOS / 1_000_000),
            Self::Mailbox { command, interface } => {
                write!(f, "pcode did not answer command {command:#04x}: the mailbox reads {interface:#010x}")
            }
            Self::LineTime { computed, firmware } => {
                write!(f, "the line time computed, {computed}, is not the firmware's {firmware}")
            }
            Self::LevelZero => write!(f, "watermark level 0 does not fit the plane's allocation"),
            Self::SagvIntolerant => write!(f, "the plane cannot tolerate SAGV's block time"),
            Self::Revision(revision) => write!(f, "PCI revision {revision:#04x} is no display stepping Vol 4 names"),
            Self::NoProgram(s) => {
                write!(f, "the DMC package has no main program for display stepping {}{}", s.letter as char, s.digit as char)
            }
            Self::Port(ddi) => write!(f, "transcoder A drives DDI select {ddi:#x}, not DDI A, whose PHY alone DC6 preserves"),
            Self::Vga => write!(f, "VGA display is enabled"),
            Self::NotStored { at, wrote, read } => {
                write!(f, "the CSR storage at {at:#x} reads {read:#010x}, not the {wrote:#010x} written")
            }
            Self::PowerWellHeld(well) => write!(f, "power well {well} stayed on with its requests withdrawn"),
            Self::Interrupts(pending) => write!(f, "DISPLAY_INT_CTL names pending categories {pending:#010x} beyond pipe A's"),
            Self::PsrOn => write!(f, "PSR or PSR2 is already enabled"),
            Self::PsrAuxError => write!(f, "PSR_IIR names a PSR AUX error"),
            Self::DelayedVblank { start, active } => {
                write!(f, "vertical blank starts at line {start}, past vertical active's {active}")
            }
            Self::WatermarkLevelOff(level) => write!(f, "watermark level {level} is disabled"),
            Self::AuxPower => write!(f, "AUX A's IO power did not come on"),
            Self::Aux(fault) => write!(f, "an AUX transaction with the panel failed: {fault:?}"),
            Self::NoReceiver => write!(f, "the panel's DPCD revision reads 0"),
            Self::NoPsr => write!(f, "the panel names no PSR version"),
            Self::NoSetPower => write!(f, "the panel cannot be set to D0"),
            Self::SinkSetupReserved => write!(f, "the panel's PSR setup time is the reserved value"),
            Self::SinkErrors { errors, status } => {
                write!(f, "the panel reports PSR errors {errors:#04x}, status {status:#04x}")
            }
            Self::SetupTime { lines, vblank } => {
                write!(f, "the panel's PSR setup takes {lines} lines of a {vblank}-line vertical blank")
            }
        }
    }
}

/// The firmware's scan-out as this driver found it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scanout {
    /// Which of pipe A's planes, from 1.
    pub plane: usize,
    pub width: u64,
    pub height: u64,
    pub bytes_per_pixel: u64,
    pub tiling: Tiling,
    /// Blocks, from PLANE_BUF_CFG, whose Buffer End is inclusive: Vol 12
    /// "Display Buffer Allocation" gives blocks 0-159 as 0x009F0000.
    pub allocation: u64,
    pub htotal: u64,
    pub vtotal: u64,
}

/// Pipe A and the one plane on it the firmware enabled; every other pipe,
/// plane and the cursor off.
pub fn scanout<R: Registers>(regs: &R) -> Result<Scanout, Refusal> {
    if regs.read32(regs::trans_conf(0)) & regs::TRANSCODER_STATE == 0 {
        return Err(Refusal::NotOnePlane("transcoder A is off"));
    }
    if (1..regs::PIPES).any(|pipe| regs.read32(regs::trans_conf(pipe)) & regs::TRANSCODER_STATE != 0) {
        return Err(Refusal::NotOnePlane("another transcoder is on"));
    }
    if regs.read32(regs::CUR_CTL_A) & regs::CURSOR_MODE != 0 {
        return Err(Refusal::NotOnePlane("the cursor is on"));
    }
    let mut enabled = (1..=regs::PLANES).filter(|n| regs.read32(regs::plane_ctl(0, *n)) & regs::PLANE_ENABLE != 0);
    let (Some(plane), None) = (enabled.next(), enabled.next()) else {
        return Err(Refusal::NotOnePlane("not exactly one plane is enabled"));
    };
    let ctl = regs.read32(regs::plane_ctl(0, plane));
    // Vol 2c PLANE_CTL "Source Pixel Format": the packed RGB formats' sizes.
    let bytes_per_pixel = match (ctl >> 23) & 0x1F {
        0b00100 | 0b01000 | 0b10100 => 4,
        0b11100 => 2,
        0b01100 => 8,
        format => return Err(Refusal::Format(format)),
    };
    // Vol 2c PLANE_CTL: Plane Rotation 1:0 (01b 90, 11b 270), Render
    // Decomp 15, Media Decomp 4.
    if ctl & 0b01 != 0 || ctl & (1 << 15 | 1 << 4) != 0 {
        return Err(Refusal::Layout(ctl));
    }
    let tiling = match (ctl >> 10) & 0x7 {
        0b000 => Tiling::Linear,
        0b001 => Tiling::X,
        0b100 => Tiling::Y,
        tiling => return Err(Refusal::Tiling(tiling)),
    };
    let size = regs.read32(regs::plane_size(0, plane));
    let buffer = regs.read32(regs::plane_buf_cfg(0, plane));
    let (start, end) = (u64::from(buffer & 0x7FF), u64::from((buffer >> 16) & 0x7FF));
    let total = |at| u64::from((regs.read32(at) >> regs::TOTAL_SHIFT) & regs::TOTAL) + 1;
    Ok(Scanout {
        plane,
        width: u64::from(size & 0x1FFF) + 1,
        height: u64::from((size >> 16) & 0x1FFF) + 1,
        bytes_per_pixel,
        tiling,
        allocation: (end + 1).saturating_sub(start),
        htotal: total(regs::TRANS_HTOTAL_A),
        vtotal: total(regs::TRANS_VTOTAL_A),
    })
}

/// Pipe A's pixel rate in kHz, measured as `htotal × vtotal` pixels a frame
/// over [`MEASURED_FRAMES`] frames of the frame counter, between two of its
/// edges. The clock's programming (its PLL) is not read: this is the rate the
/// mode actually runs at, which is what the watermarks follow, and
/// [`keep_memory_low_power`] holds it against the firmware's own line time.
pub fn pixel_rate_khz<R: Registers, C: Clock>(regs: &R, clock: &C, scanout: &Scanout) -> Result<u64, Refusal> {
    // The count and the time it was seen at, read together, so an edge is
    // never counted against another edge's time.
    let next_frame = |after: u32| -> Result<(u32, u64), Refusal> {
        let deadline = clock.nanos() + FRAME_CEILING_NANOS;
        loop {
            let (count, at) = (regs.read32(regs::PIPE_FRMCNT_A), clock.nanos());
            if count != after {
                return Ok((count, at));
            }
            if at > deadline {
                return Err(Refusal::PipeStopped);
            }
            clock.pause(FRAME_POLL_NANOS);
        }
    };
    let (base, start) = next_frame(regs.read32(regs::PIPE_FRMCNT_A))?;
    let (mut seen, mut end) = (base, start);
    while seen.wrapping_sub(base) < MEASURED_FRAMES {
        (seen, end) = next_frame(seen)?;
    }
    let frames = u64::from(seen.wrapping_sub(base));
    let pixels = scanout.htotal * scanout.vtotal * frames;
    // kHz is pixels a millisecond.
    Ok(pixels * 1_000_000 / (end - start).max(1))
}

/// One GT driver mailbox command (Vol 12 "Mailbox Access Routine"): its Data0,
/// or the interface word it was refused with.
fn mailbox<R: Registers, C: Clock>(regs: &R, clock: &C, command: u32, data0: u32) -> Result<u32, Refusal> {
    let idle = || regs.read32(regs::MAILBOX_INTERFACE) & regs::RUN_BUSY == 0;
    let wait = || {
        let deadline = clock.nanos() + MAILBOX_NANOS;
        while !idle() && clock.nanos() <= deadline {}
        idle()
    };
    let refused = || Refusal::Mailbox { command, interface: regs.read32(regs::MAILBOX_INTERFACE) };
    if !wait() {
        return Err(refused());
    }
    regs.write32(regs::MAILBOX_DATA0, data0);
    regs.write32(regs::MAILBOX_DATA1, 0);
    regs.write32(regs::MAILBOX_INTERFACE, regs::RUN_BUSY | command);
    // "Poll ... for Run/Busy indication=0b and Command/Error Code=00h (success)"
    if !wait() || regs.read32(regs::MAILBOX_INTERFACE) & 0xFF != 0 {
        return Err(refused());
    }
    Ok(regs.read32(regs::MAILBOX_DATA0))
}

/// Vol 12 "Retrieve Memory Latency Data" (command 06h, Data0 0 then 1) and
/// "SAGV Block Time" (command 23h).
pub fn memory<R: Registers, C: Clock>(regs: &R, clock: &C) -> Result<(Latencies, u64), Refusal> {
    let first = mailbox(regs, clock, 0x06, 0)?;
    let second = mailbox(regs, clock, 0x06, 1)?;
    let block = mailbox(regs, clock, 0x23, 0)?;
    Ok((Latencies::from_pcode(first, second), u64::from(block)))
}

/// What [`keep_memory_low_power`] found and did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Programmed {
    pub scanout: Scanout,
    pub pixel_rate_khz: u64,
    pub latencies: Latencies,
    pub sagv_block_us: u64,
    pub watermarks: Watermarks,
    /// PLANE_WM levels 0-7 as the firmware left them.
    pub before: [u32; regs::LEVELS],
}

impl Programmed {
    /// One frame of the measured mode.
    pub fn refresh_interval_nanos(&self) -> u64 {
        self.scanout.htotal * self.scanout.vtotal * 1_000_000 / self.pixel_rate_khz
    }
}

/// Watermark levels 0-7 for the firmware's plane, programmed and armed (Vol 12
/// "Watermark Algorithm" step 5).
///
/// The firmware's line time stays: it must be programmed before any low power
/// level is enabled (Vol 2c WM_LINETIME), and it is held against the one
/// computed here. SAGV stays as the firmware left it, all points unrestricted
/// by default (Vol 12 "SAGV Point Selection"), where the plane tolerates it;
/// where it does not, nothing is written. The transition watermark stays
/// disabled: it is a tunable for Isochronous Priority Control, and disabled it
/// behaves as the top of the allocation (Vol 12 "Transition Watermark").
pub fn keep_memory_low_power<R: Registers, C: Clock>(regs: &R, clock: &C) -> Result<Programmed, Refusal> {
    let scanout = scanout(regs)?;
    let pixel_rate_khz = pixel_rate_khz(regs, clock, &scanout)?;
    let pipe = Pipe { htotal: scanout.htotal, pixel_rate_khz };
    let firmware = regs.read32(regs::WM_LINETIME_A) & regs::LINE_TIME;
    let computed = watermark::line_time(&pipe);
    // Within one: the reference rounds up in Vol 12 and to nearest in Vol 2c.
    if computed.abs_diff(u64::from(firmware)) > 1 {
        return Err(Refusal::LineTime { computed, firmware });
    }
    let (latencies, sagv_block_us) = memory(regs, clock)?;
    let plane = Plane {
        width: scanout.width,
        bytes_per_pixel: scanout.bytes_per_pixel,
        tiling: scanout.tiling,
        allocation: scanout.allocation,
    };
    let watermarks = watermark::watermarks(&latencies, sagv_block_us, &plane, &pipe).ok_or(Refusal::LevelZero)?;
    if !watermarks.tolerates_sagv {
        return Err(Refusal::SagvIntolerant);
    }
    let before = core::array::from_fn(|level| regs.read32(regs::plane_wm(0, scanout.plane, level)));
    for (level, word) in watermarks.registers().into_iter().enumerate() {
        regs.write32(regs::plane_wm(0, scanout.plane, level), word);
    }
    // The surface written again, unchanged: the arm for the double-buffered
    // watermarks, which take effect at the next vertical blank.
    let surface = regs::plane_surf(0, scanout.plane);
    regs.write32(surface, regs.read32(surface));
    Ok(Programmed { scanout, pixel_rate_khz, latencies, sagv_block_us, watermarks, before })
}

/// What [`allow_display_c6`] found and did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Allowed {
    /// PWR_WELL_CTL1, PWR_WELL_CTL2 and DC_STATE_EN as the firmware left them.
    pub before: [u32; 3],
    /// TRANS_DDI_FUNC_CTL_A's DDI Select.
    pub ddi: u32,
}

/// Vol 12 "Sequence to Allow DC5 or DC6", up to DC6, under the firmware's
/// eDP scan-out on pipe A and DDI A.
///
/// DC states are disallowed first, since a switch between DC5 and DC6 goes
/// through "Disable" and a reload. Then `program` is loaded, the wells the
/// scan-out does not use are powered down and DC6 is allowed. With the pipe
/// lit, Vol 12 lets the engine enter DC5 only while PSR is active. A DDI AUX
/// transaction or a mode set must disallow DC5 and DC6 around it (the same
/// section).
///
/// The refusals before the load write nothing. One after it leaves DC states
/// disallowed, and whatever wells were already withdrawn stay so.
pub fn allow_display_c6<R: Registers, C: Clock>(regs: &R, clock: &C, program: &dmc::Program<'_>) -> Result<Allowed, Refusal> {
    let before = [regs::PWR_WELL_CTL1, regs::PWR_WELL_CTL2, regs::DC_STATE_EN].map(|at| regs.read32(at));
    let ddi = (regs.read32(regs::TRANS_DDI_FUNC_CTL_A) >> regs::DDI_SELECT_SHIFT) & regs::DDI_SELECT;
    if ddi != regs::DDI_A {
        return Err(Refusal::Port(ddi));
    }
    if regs.read32(regs::VGA_CONTROL) & regs::VGA_DISPLAY_DISABLE == 0 {
        return Err(Refusal::Vga);
    }
    let dc = regs.read32(regs::DC_STATE_EN);
    if dc & regs::DYNAMIC_DC_STATE != 0 {
        regs.write32(regs::DC_STATE_EN, dc & !regs::DYNAMIC_DC_STATE);
    }
    dmc::load(regs, program).map_err(|(at, wrote, read)| Refusal::NotStored { at, wrote, read })?;
    for well in UNUSED_WELLS {
        power_down(regs, clock, well)?;
    }
    // Vol 12 "Audio Bios Programming Sequence": "GFX driver disables
    // AUDIO_PIN_BUF_CTL bit 31 after turning off the power well for audio
    // (PG3)".
    let pins = regs.read32(regs::AUDIO_PIN_BUF_CTL);
    if pins & regs::AUDIO_BUFFER_ENABLE != 0 {
        regs.write32(regs::AUDIO_PIN_BUF_CTL, pins & !regs::AUDIO_BUFFER_ENABLE);
    }
    regs.write32(regs::DC_PREREQUISITE, regs.read32(regs::DC_PREREQUISITE) | regs::DC_PREREQUISITE_BITS);
    let dc = regs.read32(regs::DC_STATE_EN);
    regs.write32(regs::DC_STATE_EN, dc & !regs::DYNAMIC_DC_STATE | regs::UP_TO_DC6);
    Ok(Allowed { before, ddi })
}

/// Power well `well`'s requests withdrawn from both instances Vol 2c names,
/// and its state awaited (Vol 12 "Power Well" disable sequence). The verdict
/// past the deadline is a read made after it, so a wait preempted past the
/// deadline is not called a held well.
fn power_down<R: Registers, C: Clock>(regs: &R, clock: &C, well: u32) -> Result<(), Refusal> {
    let request = regs::power_well_request(well);
    for ctl in [regs::PWR_WELL_CTL1, regs::PWR_WELL_CTL2] {
        let value = regs.read32(ctl);
        if value & request != 0 {
            regs.write32(ctl, value & !request);
        }
    }
    let deadline = clock.nanos() + POWER_WELL_NANOS;
    loop {
        let late = clock.nanos() > deadline;
        if regs.read32(regs::PWR_WELL_CTL2) & regs::power_well_state(well) == 0 {
            return Ok(());
        }
        if late {
            return Err(Refusal::PowerWellHeld(well));
        }
    }
}
