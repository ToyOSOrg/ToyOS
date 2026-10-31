//! Every decision the Tiger Lake display driver makes, against
//! [`Registers`] and a [`Clock`], from Intel's published references:
//! IHD-OS-TGL-Vol 12-12.21 (Display Engine) and IHD-OS-TGL-Vol 2c-12.21
//! (registers). Each register and sequence is cited where it is used.
//!
//! **What it does today is power on the firmware's surface**
//! ([`keep_memory_low_power`]): the mode, the plane and its buffer are the
//! GOP's and stay so. It reads the one plane the firmware scans out, pcode's
//! memory latencies and SAGV block time, computes watermark levels 0-7 by
//! Vol 12's algorithm ([`watermark`]) and programs them, so the display lets
//! memory reach the latencies the levels name. Anything it does not
//! understand of the firmware's state is refused by name and left as found.

#![no_std]
#![forbid(unsafe_code)]

use core::fmt;

use toyos_device_memory::Registers;

pub mod regs;
pub mod watermark;

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
const FRAME_CEILING_NANOS: u64 = 84_000_000;
/// Between two reads of the frame counter.
const FRAME_POLL_NANOS: u64 = 50_000;
/// Vol 12 "Retrieve Memory Latency Data" and "SAGV Block Time": "Timeout after
/// 100 us and do not enable display planes."
const MAILBOX_NANOS: u64 = 100_000;

/// Why the firmware's state was left as found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The firmware's state is not one pipe A plane scanning out alone.
    NotOnePlane(&'static str),
    /// The plane's pixel format, PLANE_CTL 27:23, is not a packed RGB one.
    Format(u32),
    /// The plane's tiling, PLANE_CTL 12:10, is not one Vol 2c names.
    Tiling(u32),
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
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotOnePlane(why) => write!(f, "the firmware's display is not one plane on pipe A: {why}"),
            Self::Format(format) => write!(f, "the plane's pixel format {format:#07b} is not packed RGB"),
            Self::Tiling(tiling) => write!(f, "the plane's tiling {tiling:#05b} is not one Vol 2c names"),
            Self::PipeStopped => write!(f, "pipe A's frame counter did not advance within {} ms", FRAME_CEILING_NANOS / 1_000_000),
            Self::Mailbox { command, interface } => {
                write!(f, "pcode did not answer command {command:#04x}: the mailbox reads {interface:#010x}")
            }
            Self::LineTime { computed, firmware } => {
                write!(f, "the line time computed, {computed}, is not the firmware's {firmware}")
            }
            Self::LevelZero => write!(f, "watermark level 0 does not fit the plane's allocation"),
            Self::SagvIntolerant => write!(f, "the plane cannot tolerate SAGV's block time"),
        }
    }
}

/// The firmware's scan-out as this driver found it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scanout {
    /// Which of pipe A's planes, from 1.
    pub plane: usize,
    pub width: u64,
    pub bytes_per_pixel: u64,
    pub tiling: Tiling,
    /// Blocks, from PLANE_BUF_CFG; the end is not counted, so a reading of
    /// it as inclusive allocates one block more than assumed here.
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
        bytes_per_pixel,
        tiling,
        allocation: end.saturating_sub(start),
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
    let next_frame = |after: u32| -> Result<u64, Refusal> {
        let deadline = clock.nanos() + FRAME_CEILING_NANOS;
        loop {
            if regs.read32(regs::PIPE_FRMCNT_A) != after {
                return Ok(clock.nanos());
            }
            if clock.nanos() > deadline {
                return Err(Refusal::PipeStopped);
            }
            clock.pause(FRAME_POLL_NANOS);
        }
    };
    let start = next_frame(regs.read32(regs::PIPE_FRMCNT_A))?;
    let base = regs.read32(regs::PIPE_FRMCNT_A);
    let (mut end, mut seen) = (start, base);
    while seen.wrapping_sub(base) < MEASURED_FRAMES {
        end = next_frame(seen)?;
        seen = regs.read32(regs::PIPE_FRMCNT_A);
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
    for (level, computed) in watermarks.levels.iter().enumerate() {
        regs.write32(regs::plane_wm(0, scanout.plane, level), computed.map_or(0, |l| l.register()));
    }
    // The surface written again, unchanged: the arm for the double-buffered
    // watermarks, which take effect at the next vertical blank.
    let surface = regs::plane_surf(0, scanout.plane);
    regs.write32(surface, regs.read32(surface));
    Ok(Programmed { scanout, pixel_rate_khz, latencies, sagv_block_us, watermarks, before })
}
