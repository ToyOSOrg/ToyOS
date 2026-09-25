//! An extent read off a block device in bounded chunks, each one said as it
//! lands, and the first that fails named and nothing read after it.
//!
//! **Why bounded.** Firmware Block I/O takes a request of any size and reports
//! no largest one it serves (UEFI 2.11 §13.9's `EFI_BLOCK_IO_MEDIA` carries an
//! alignment and, from revision 3, an optimal granularity, and no maximum), so
//! the bound is the caller's to choose, and [`chunk_bytes`] chooses it.
//!
//! **Why said.** Block I/O takes no timeout and says nothing while a request is
//! outstanding, so a request that never returns leaves only what was said
//! before it. [`read`] hands its caller a [`Progress`] at each tenth crossed,
//! before it asks for the next chunk.
//!
//! Pure: no `alloc`, no firmware; the loader supplies the device.

#![no_std]
#![forbid(unsafe_code)]

/// A device that reads whole logical blocks starting at an LBA.
pub trait Blocks {
    type Error;
    /// Fill `into`, a whole number of logical blocks, from `lba` onwards.
    fn read(&mut self, lba: u64, into: &mut [u8]) -> Result<(), Self::Error>;
}

/// The chunk that failed, and how much of the extent was read before it.
#[derive(Debug, PartialEq, Eq)]
pub struct Failed<E> {
    /// The chunk's first LBA.
    pub lba: u64,
    /// The chunk's length in logical blocks.
    pub blocks: u64,
    /// Bytes of the extent read before this chunk, every one of them good.
    pub read: usize,
    pub error: E,
}

/// How far [`read`] has got: `tenths` of the extent, `read` bytes of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub tenths: u32,
    pub read: usize,
}

/// The chunk length to read with: the largest multiple of `align` and of the
/// media's optimal granularity that is at most `bound`.
///
/// `align` is every chunk's offset from the start of the buffer, so a buffer
/// aligned to `align` is handed to the device as chunks aligned to it too.
/// `granularity_lbas` is `OptimalTransferLengthGranularity`, 0 for media that
/// report none; a granularity whose unit exceeds `bound` is advice this does
/// not take, because the bound is the point.
///
/// # Panics
/// On arguments that are the caller's constants and are wrong: `align` not a
/// power of two, `lba_bytes` not dividing it, or `bound` not a multiple of it.
pub fn chunk_bytes(bound: usize, align: usize, lba_bytes: u32, granularity_lbas: u32) -> usize {
    assert!(align.is_power_of_two(), "alignment {align} is not a power of two");
    assert!(lba_bytes != 0 && align.is_multiple_of(lba_bytes as usize), "{lba_bytes}-byte blocks do not divide {align}");
    assert!(bound >= align && bound.is_multiple_of(align), "bound {bound} is not a multiple of {align}");
    let unit = (granularity_lbas as usize)
        .checked_mul(lba_bytes as usize)
        .filter(|&granule| granule != 0)
        .and_then(|granule| lcm(align, granule))
        .filter(|&unit| unit <= bound)
        .unwrap_or(align);
    bound / unit * unit
}

fn lcm(a: usize, b: usize) -> Option<usize> {
    let (mut x, mut y) = (a, b);
    while y != 0 {
        (x, y) = (y, x % y);
    }
    (a / x).checked_mul(b)
}

/// Read `into.len()` bytes from `first_lba` onwards, `chunk` bytes per request
/// and the last request whatever is left, calling `progress` each time the
/// share read crosses a tenth.
///
/// # Panics
/// When `chunk` or `into.len()` is not a whole number of `lba_bytes` blocks,
/// or `chunk` is zero: those are the caller's arithmetic, not the device's.
pub fn read<B: Blocks>(
    device: &mut B,
    first_lba: u64,
    lba_bytes: u32,
    chunk: usize,
    into: &mut [u8],
    mut progress: impl FnMut(Progress),
) -> Result<(), Failed<B::Error>> {
    let lba_bytes = lba_bytes as usize;
    assert!(lba_bytes != 0 && chunk != 0 && chunk.is_multiple_of(lba_bytes), "a {chunk}-byte chunk of {lba_bytes}-byte blocks");
    assert!(into.len().is_multiple_of(lba_bytes), "a {}-byte extent of {lba_bytes}-byte blocks", into.len());
    let len = into.len();
    let mut said = 0;
    for (index, piece) in into.chunks_mut(chunk).enumerate() {
        let read = index * chunk;
        let lba = first_lba + (read / lba_bytes) as u64;
        let blocks = (piece.len() / lba_bytes) as u64;
        device.read(lba, piece).map_err(|error| Failed { lba, blocks, read, error })?;
        let read = read + piece.len();
        let tenths = (read as u128 * 10 / len as u128) as u32;
        if tenths > said {
            said = tenths;
            progress(Progress { tenths, read });
        }
    }
    Ok(())
}
