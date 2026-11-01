//! Vol 12 (IHD-OS-TGL-Vol 12-12.21) "Watermark Algorithm" steps 3-5 for one
//! plane, and "SAGV Interaction with Watermarks", in exact integer arithmetic:
//! every quantity the algorithm takes a ceiling of is held as a fraction.
//!
//! Scaling, interlacing, 90/270 rotation and YUV 4:2:0 are outside what
//! this driver programs, so a plane here is unscaled, at 0 or 180 degrees and
//! packed; [`crate::scanout`] refuses any other.

use crate::regs::LEVELS;

/// Vol 12: "All other tiling and surface formats", 512 bytes. The 256-byte
/// block is 8 bpp with Yf tiling, which Tiger Lake's planes do not take.
const BLOCK_BYTES: u64 = 512;
/// Vol 12, "Minimum Scanlines for Y Tile" at 0/180 rotation, for every
/// bytes per pixel this driver programs.
const Y_TILE_LINES: u64 = 4;
/// Vol 12, step 3.VI: "Maximum Lines: 31".
const MAX_LINES: u64 = 31;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tiling {
    Linear,
    X,
    Y,
}

/// One enabled plane as the algorithm reads it.
#[derive(Clone, Copy, Debug)]
pub struct Plane {
    /// Source width in pixels.
    pub width: u64,
    pub bytes_per_pixel: u64,
    pub tiling: Tiling,
    /// Its DBUF allocation in blocks (PLANE_BUF_CFG).
    pub allocation: u64,
}

/// The pipe the plane is on.
#[derive(Clone, Copy, Debug)]
pub struct Pipe {
    /// Horizontal total in pixels.
    pub htotal: u64,
    /// The adjusted pipe pixel rate, in kHz.
    pub pixel_rate_khz: u64,
}

/// A memory latency level's watermark, as PLANE_WM holds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Level {
    pub blocks: u64,
    pub lines: u64,
}

impl Level {
    /// PLANE_WM with Enable set.
    pub fn register(self) -> u32 {
        crate::regs::WM_ENABLE | (self.lines as u32) << 14 | self.blocks as u32
    }
}

/// `n / d`, never reduced.
#[derive(Clone, Copy, Debug)]
struct Frac(u64, u64);

impl Frac {
    fn ceil(self) -> u64 {
        self.0.div_ceil(self.1)
    }
    fn max(self, other: Self) -> Self {
        if self.0 * other.1 >= other.0 * self.1 {
            self
        } else {
            other
        }
    }
    fn scale(self, k: u64) -> Self {
        Frac(self.0 * k, self.1)
    }
    /// `self / other`.
    fn over(self, other: Self) -> Self {
        Frac(self.0 * other.1, self.1 * other.0)
    }
}

/// Vol 12, step 3.B.II: plane blocks per line.
fn blocks_per_line(plane: &Plane) -> Frac {
    let bytes_per_line = plane.width * plane.bytes_per_pixel;
    match plane.tiling {
        Tiling::Linear | Tiling::X => Frac(bytes_per_line.div_ceil(BLOCK_BYTES) + 1, 1),
        Tiling::Y => Frac((Y_TILE_LINES * bytes_per_line).div_ceil(BLOCK_BYTES) + 1, Y_TILE_LINES),
    }
}

/// Method 1, Vol 12 step 3.B.I: latency × rate × Bpp / block + 1.
fn method_1(latency_us: u64, plane: &Plane, pipe: &Pipe) -> Frac {
    let d = 1000 * BLOCK_BYTES;
    Frac(latency_us * pipe.pixel_rate_khz * plane.bytes_per_pixel + d, d)
}

/// Method 2, Vol 12 step 3.B.II: ceiling[latency × rate / htotal] × blocks per line.
fn method_2(latency_us: u64, plane: &Plane, pipe: &Pipe) -> Frac {
    let lines = (latency_us * pipe.pixel_rate_khz).div_ceil(1000 * pipe.htotal);
    blocks_per_line(plane).scale(lines)
}

/// One level's watermark at `latency_us`, or `None` where it exceeds the
/// maximum (Vol 12 steps 3.B.IV-VI).
pub fn level(latency_us: u64, plane: &Plane, pipe: &Pipe) -> Option<Level> {
    let per_line = blocks_per_line(plane);
    let selected = match plane.tiling {
        Tiling::Linear | Tiling::X => {
            let bytes_per_line = plane.width * plane.bytes_per_pixel;
            let tiny = plane.bytes_per_pixel * pipe.htotal < BLOCK_BYTES && bytes_per_line < BLOCK_BYTES;
            // "plane buffer allocation / plane blocks per line >= 1"
            let a_line_fits = plane.allocation * per_line.1 >= per_line.0;
            // "memory latency microseconds >= line time microseconds",
            // line time being htotal / rate MHz.
            let past_a_line = latency_us * pipe.pixel_rate_khz >= pipe.htotal * 1000;
            if tiny || a_line_fits || past_a_line {
                method_2(latency_us, plane, pipe)
            } else {
                method_1(latency_us, plane, pipe)
            }
        }
        Tiling::Y => method_2(latency_us, plane, pipe).max(per_line.scale(Y_TILE_LINES)),
    };
    let blocks = selected.ceil() + 1;
    let lines = selected.over(per_line).ceil();
    let needed = match plane.tiling {
        Tiling::Linear | Tiling::X => (blocks * 11).div_ceil(10),
        Tiling::Y => {
            let extra = if lines % Y_TILE_LINES == 0 { Y_TILE_LINES } else { 2 * Y_TILE_LINES - lines % Y_TILE_LINES };
            per_line.scale(lines + extra).ceil()
        }
    };
    (blocks < plane.allocation && lines <= MAX_LINES && needed < plane.allocation).then_some(Level { blocks, lines })
}

/// The memory latencies pcode reports (Vol 12 "Memory Latency Data
/// Definition"), in µs: level 0, which is always valid, and the levels above,
/// `None` from the first invalid one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Latencies {
    pub zero: u64,
    pub above: [Option<u64>; LEVELS - 1],
}

impl Latencies {
    /// From the two Data0 words, level 0 in the first word's low byte. "If
    /// level 1 or any higher level has a value of 0x00, that level and any
    /// higher levels are unused and invalid"; and "if the mailbox response
    /// data for level 0 is 0us, add 3 microseconds to the result for each
    /// valid level".
    pub fn from_pcode(first: u32, second: u32) -> Self {
        let raw = |i: usize| u64::from((if i < 4 { first } else { second } >> (8 * (i % 4))) & 0xFF);
        let add = if raw(0) == 0 { 3 } else { 0 };
        let mut above = [None; LEVELS - 1];
        for (i, level) in above.iter_mut().enumerate() {
            match raw(i + 1) {
                0 => break,
                us => *level = Some(us + add),
            }
        }
        Self { zero: raw(0) + add, above }
    }
}

/// A plane's watermarks: level 0, each level above enabled or not, and
/// whether it tolerates SAGV.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Watermarks {
    pub zero: Level,
    pub above: [Option<Level>; LEVELS - 1],
    /// Level 0 was computed with the SAGV block time added and fits.
    pub tolerates_sagv: bool,
}

impl Watermarks {
    /// PLANE_WM for each level, 0 first; a disabled level is 0.
    pub fn registers(&self) -> [u32; LEVELS] {
        core::array::from_fn(|i| match i {
            0 => self.zero.register(),
            i => self.above[i - 1].map_or(0, Level::register),
        })
    }
}

/// Every level for `plane` on `pipe` (Vol 12 step 5A, and "SAGV Interaction
/// with Watermarks"). Level 0 takes the SAGV block time where the result can
/// be supported; each higher level's latency is then raised to at least the
/// one below it, since "the latency input to the watermark calculation for
/// each level must be greater than or equal to the lower level". A level is
/// enabled only where it and every level below it fit.
///
/// `None` where level 0 does not fit even without SAGV: "the plane must not
/// be enabled".
pub fn watermarks(latencies: &Latencies, sagv_block_us: u64, plane: &Plane, pipe: &Pipe) -> Option<Watermarks> {
    let with_sagv = level(latencies.zero + sagv_block_us, plane, pipe);
    let tolerates_sagv = with_sagv.is_some();
    let (zero, mut below) = match with_sagv {
        Some(zero) => (zero, latencies.zero + sagv_block_us),
        None => (level(latencies.zero, plane, pipe)?, latencies.zero),
    };
    let mut above = [None; LEVELS - 1];
    for (slot, latency) in above.iter_mut().zip(latencies.above) {
        let Some(latency) = latency else { break };
        let latency = latency.max(below);
        let Some(computed) = level(latency, plane, pipe) else { break };
        *slot = Some(computed);
        below = latency;
    }
    Some(Watermarks { zero, above, tolerates_sagv })
}

/// Vol 12 step 2B: WM_LINETIME's line time, roundup[8 × htotal / rate MHz].
pub fn line_time(pipe: &Pipe) -> u64 {
    (8 * pipe.htotal * 1000).div_ceil(pipe.pixel_rate_khz)
}
