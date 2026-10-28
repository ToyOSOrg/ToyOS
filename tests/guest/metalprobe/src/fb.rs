//! What the scanout answers about itself: that a pattern written to it reads
//! back as itself, and at what rate each direction runs.
//!
//! **The two directions are not one measurement.** The scanout is mapped
//! write-combining (`kernel/src/drivers/gop.rs` picks the memory type and says
//! so in a record), so a write lands in the fill buffers and a read misses
//! every cache — which is why `sdk/window` has no read path at all.
//! The rate each way is a fact about the machine's PAT entry and its memory
//! controller, and no emulated display can answer it.

use std::time::Instant;

use toyos::device::FramebufferDev;
use toyos::endow::Endowments;
use toyos::syscap::SysCap;
use toyos_abi::syscall::{DeviceType, SYSCAP_LABEL};

use crate::{span, Measured, Refusal};

/// How many whole-scanout passes each rate is measured over.
///
/// **Two counts, because the two directions are orders of magnitude apart.**
/// A fill runs at the memory controller's rate and needs several passes before
/// the millisecond clock has anything to divide; a readback misses every cache,
/// so one pass is already a long span and the job list's own bound
/// (`toyos_tco::JOB_BOUND_MS`) is what caps how many there may be.
const FILL_PASSES: u64 = 8;
const READ_PASSES: u64 = 2;

/// The pattern a pixel carries, derived from where it is so that a row written
/// to the wrong offset changes the fold.
///
/// The two odd multipliers are the 32-bit halves of a Fibonacci hash; what
/// matters is only that every coordinate bit reaches the top of the word, so a
/// stride the driver reported wrongly cannot fold to the same value.
fn pixel(x: u32, y: u32) -> u32 {
    let mixed = x.wrapping_mul(0x9E37_79B9) ^ y.wrapping_mul(0x85EB_CA6B).rotate_left(13);
    mixed ^ (mixed >> 15)
}

/// The scanout this process may write, and the shape the driver described it
/// with.
struct Scanout {
    /// The claim and its mapping: dropping the claim gives the display back.
    opened: toyos::device::Scanout,
    width: u32,
    height: u32,
    /// Pixels per row, which is not the width on a firmware framebuffer.
    stride: u32,
}

impl Scanout {
    fn claim() -> Result<Self, Refusal> {
        let cap: SysCap = Endowments::get().take(SYSCAP_LABEL).ok_or(Refusal::NoCapability)?;
        let dev: FramebufferDev =
            cap.claim(DeviceType::Framebuffer).map_err(|_| Refusal::NoDevice)?;
        let opened = dev.open().map_err(|_| Refusal::NoScanout)?;
        let info = opened.info();
        if info.width == 0 || info.height == 0 || info.stride < info.width {
            return Err(Refusal::NoScanout);
        }
        let (width, height, stride) = (info.width, info.height, info.stride);
        Ok(Self { opened, width, height, stride })
    }

    /// One row's worth of the pattern, in the row's own coordinates.
    fn row(&self, y: u32) -> Vec<u32> {
        (0..self.width).map(|x| pixel(x, y)).collect()
    }

    /// Copy `row` into row `y` of the scanout. A blit, not a word loop: what is
    /// measured has to be the path a compositor takes.
    fn put(&self, y: u32, row: &[u32]) {
        // SAFETY: a `u32` slice's bytes, every one initialised.
        let bytes = unsafe { std::slice::from_raw_parts(row.as_ptr().cast::<u8>(), size_of_val(row)) };
        self.opened.memory().bytes().write(self.offset(y), bytes);
    }

    /// Row `y` of the scanout, copied out into ordinary memory.
    fn get(&self, y: u32, into: &mut [u32]) {
        // SAFETY: as `put`; any bytes are a valid `u32`.
        let bytes = unsafe { std::slice::from_raw_parts_mut(into.as_mut_ptr().cast::<u8>(), size_of_val(into)) };
        self.opened.memory().bytes().read(self.offset(y), bytes);
    }

    fn offset(&self, y: u32) -> usize {
        y as usize * self.stride as usize * 4
    }

    /// Every write is in the fill buffers until this runs: a weakly-ordered
    /// mapping owes a fence before anything reads what was put there.
    fn fence(&self) {
        crate::arch::drain_stores();
    }

    /// Paint the whole scanout black, so the kernel's panel is legible again
    /// for whatever the rest of the boot has to say on a machine with no
    /// serial port.
    fn clear(&self) {
        let black = vec![0u32; self.width as usize];
        for y in 0..self.height {
            self.put(y, &black);
        }
        self.fence();
    }
}

/// The scanout checked against itself: the pattern written, fenced, and read
/// back through the same mapping, **word for word**.
///
/// The comparison is the verdict and it is made in the guest, where the pattern
/// is: a display that dropped a store, a driver that reported the wrong stride
/// and a mapping of the wrong memory type each answer [`Refusal::Disagreed`]
/// rather than a number. What the mode was is a kernel record (`GOP:`) and not
/// this job's to carry.
///
/// The number is the span of the write and the read together.
pub fn check() -> Measured {
    let fb = Scanout::claim()?;
    let began = Instant::now();
    for y in 0..fb.height {
        fb.put(y, &fb.row(y));
    }
    fb.fence();

    let mut got = vec![0u32; fb.width as usize];
    let mut disagreed = false;
    for y in 0..fb.height {
        fb.get(y, &mut got);
        disagreed |= got != fb.row(y);
    }
    let took = began.elapsed();
    fb.clear();
    if disagreed {
        return Err(Refusal::Disagreed);
    }
    span(took.as_nanos())
}

/// The span of [`FILL_PASSES`] whole-scanout fills.
pub fn fill() -> Measured {
    let fb = Scanout::claim()?;
    let rows: Vec<Vec<u32>> = (0..fb.height).map(|y| fb.row(y)).collect();
    // One pass before the clock so the mapping's first-touch faults are not in
    // the span being timed.
    for (y, row) in rows.iter().enumerate() {
        fb.put(y as u32, row);
    }
    fb.fence();

    let began = Instant::now();
    for _ in 0..FILL_PASSES {
        for (y, row) in rows.iter().enumerate() {
            fb.put(y as u32, row);
        }
        fb.fence();
    }
    let took = began.elapsed();
    fb.clear();
    span(took.as_nanos())
}

/// The span of [`READ_PASSES`] whole-scanout readbacks — the direction that
/// misses every cache, and the one no emulated display can answer.
pub fn read_back() -> Measured {
    let fb = Scanout::claim()?;
    for y in 0..fb.height {
        fb.put(y, &fb.row(y));
    }
    fb.fence();

    let mut got = vec![0u32; fb.width as usize];
    for y in 0..fb.height {
        fb.get(y, &mut got);
    }

    let began = Instant::now();
    let mut seen: u32 = 0;
    for _ in 0..READ_PASSES {
        for y in 0..fb.height {
            fb.get(y, &mut got);
            // Read, so the copies cannot be optimised away as unused.
            seen ^= got[0];
        }
    }
    let took = began.elapsed();
    fb.clear();
    if seen == u32::MAX && got.is_empty() {
        return Err(Refusal::Disagreed);
    }
    span(took.as_nanos())
}
