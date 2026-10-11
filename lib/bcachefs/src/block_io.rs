use core::fmt;

use crate::fs::FsError;

pub const BLOCK_SIZE: usize = 4096;

/// A block number on disk. Cannot be confused with a byte offset.
///
/// It cannot be *turned into* one here either: the conversion this type used to
/// carry was `self.0 * BLOCK_SIZE`, an unchecked multiply on a number a btree
/// inside the image chose, and nothing in the tree ever called it. [`byte_range`]
/// is the one way a block becomes bytes, and it is `checked_mul` for the reason
/// its own doc gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockNum(u64);

impl BlockNum {
    pub const fn new(n: u64) -> Self {
        Self(n)
    }

    pub const fn raw(self) -> u64 {
        self.0
    }
}

impl fmt::Display for BlockNum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "block#{}", self.0)
    }
}

/// A 4096-byte aligned block buffer. Guarantees correct size at compile time.
#[repr(C, align(4096))]
pub struct BlockBuf(pub [u8; BLOCK_SIZE]);

impl BlockBuf {
    pub fn zeroed() -> Self {
        Self([0u8; BLOCK_SIZE])
    }

    pub fn as_bytes(&self) -> &[u8; BLOCK_SIZE] {
        &self.0
    }

    pub fn as_bytes_mut(&mut self) -> &mut [u8; BLOCK_SIZE] {
        &mut self.0
    }
}

impl Default for BlockBuf {
    fn default() -> Self {
        Self::zeroed()
    }
}

/// The two variants mean opposite give-up policies. Each carries a witness only
/// this crate can mint, so rustc's fix-it for the old erasure — which
/// re-collapses a still-durable `Refused` into `Failed`, the loss #327 removed
/// — no longer compiles, nor can the witness be named outside the crate:
///
/// ```compile_fail
/// let r: Result<(), u32> = Err(7);
/// let _: Result<(), bcachefs::DeviceError> = r.map_err(|_| bcachefs::DeviceError::Failed);
/// ```
/// ```compile_fail
/// use bcachefs::Sealed;
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceError {
    /// The device's own word: the transfer was attempted and failed.
    Failed(Sealed),
    /// Refused before it was attempted, on the caller's time budget; safe to
    /// ask again.
    Refused(Sealed),
}

/// The variants' witness: unmintable (private field) and unnameable (private module) outside the crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sealed(());

/// What a foreign error answers at [`DeviceError::classify`]; the answer is the discriminant.
pub trait TransferError {
    /// True when the transfer was never attempted — refused on budget, still durable.
    fn refused_before_attempt(&self) -> bool;
}

impl DeviceError {
    pub(crate) const FAILED: Self = Self::Failed(Sealed(()));
    pub(crate) const REFUSED: Self = Self::Refused(Sealed(()));

    /// The only constructor reachable outside the crate.
    pub fn classify<E: TransferError>(err: &E) -> Self {
        if err.refused_before_attempt() {
            Self::REFUSED
        } else {
            Self::FAILED
        }
    }
}

/// The byte range block `block` occupies in an image, or `None` when that
/// range cannot be expressed at all.
///
/// **`checked_mul`, and that is not a spelling choice.** A block number reaches
/// this crate out of a btree that lives in the image it indexes, so a corrupt
/// or hostile image names any `u64` it likes — and `0x0020_0000_0000_0000 * 4096`
/// wraps. Under the kernel's build profile (`overflow-checks = true`) a wrapping
/// multiply is a *panic in the kernel* from input that crossed a trust boundary,
/// and under a release profile it is a small in-range offset into somebody
/// else's memory. Both are refusals here.
///
/// One function rather than the same three lines at each of the four call
/// sites: the bound every image-backed reader applies is this one.
fn byte_range(block: BlockNum) -> Option<(usize, usize)> {
    let off = (block.raw() as usize).checked_mul(BLOCK_SIZE)?;
    let end = off.checked_add(BLOCK_SIZE)?;
    Some((off, end))
}

/// Block-level I/O abstraction.
///
/// `&self` with interior mutability — implementations handle their own
/// synchronization. `buf` is always exactly BLOCK_SIZE bytes via BlockBuf.
///
/// Every method is fallible for the reason every [`BlockDevice`] method is: a
/// block the device would not give back is not a block of zeros, and an
/// implementation with nowhere to report that has to invent one. The kernel's
/// did — it logged and served zeros, so a read error reached the btree as a
/// block that fails its structural checks rather than as a failure.
///
/// [`BlockDevice`]: ../../kernel/src/block.rs
pub trait BlockIO {
    #[must_use = "a refused read left the buffer holding whatever it held before"]
    fn read_block(&self, block: BlockNum, buf: &mut BlockBuf) -> Result<(), DeviceError>;
    #[must_use = "a refused write did not reach the device"]
    fn write_block(&self, block: BlockNum, buf: &BlockBuf) -> Result<(), DeviceError>;
    fn block_count(&self) -> u64;
    fn sync(&self) -> Result<(), DeviceError> {
        Ok(())
    }
}

/// The same three operations, reported as [`FsError`] with the block attached.
///
/// Every call site inside this crate goes through these rather than through
/// [`BlockIO`] directly, so the block number in the error is the one the caller
/// passed in and there is no way for an implementation to name a different one.
pub(crate) trait BlockIOExt {
    fn read(&self, block: BlockNum, buf: &mut BlockBuf) -> Result<(), FsError>;
    fn write(&self, block: BlockNum, buf: &BlockBuf) -> Result<(), FsError>;
    fn flush(&self) -> Result<(), FsError>;
}

impl<T: BlockIO + ?Sized> BlockIOExt for T {
    fn read(&self, block: BlockNum, buf: &mut BlockBuf) -> Result<(), FsError> {
        self.read_block(block, buf).map_err(|e| FsError::DeviceRead(block, e))
    }

    fn write(&self, block: BlockNum, buf: &BlockBuf) -> Result<(), FsError> {
        self.write_block(block, buf).map_err(|e| FsError::DeviceWrite(block, e))
    }

    fn flush(&self) -> Result<(), FsError> {
        self.sync().map_err(FsError::DeviceSync)
    }
}

// --- Host-side implementations ---

/// In-memory block device backed by a Vec<u8>. Used by mkfs on the host.
#[cfg(feature = "std")]
pub struct VecBlockIO {
    data: std::cell::RefCell<Vec<u8>>,
}

#[cfg(feature = "std")]
impl VecBlockIO {
    pub fn new(block_count: u64) -> Self {
        let size = block_count as usize * BLOCK_SIZE;
        Self {
            data: std::cell::RefCell::new(vec![0u8; size]),
        }
    }

    pub fn from_vec(data: Vec<u8>) -> Self {
        Self {
            data: std::cell::RefCell::new(data),
        }
    }

    pub fn into_vec(self) -> Vec<u8> {
        self.data.into_inner()
    }
}

#[cfg(feature = "std")]
impl BlockIO for VecBlockIO {
    fn read_block(&self, block: BlockNum, buf: &mut BlockBuf) -> Result<(), DeviceError> {
        let data = self.data.borrow();
        let (off, end) = byte_range(block).ok_or(DeviceError::FAILED)?;
        buf.0.copy_from_slice(data.get(off..end).ok_or(DeviceError::FAILED)?);
        Ok(())
    }

    fn write_block(&self, block: BlockNum, buf: &BlockBuf) -> Result<(), DeviceError> {
        let mut data = self.data.borrow_mut();
        let (off, end) = byte_range(block).ok_or(DeviceError::FAILED)?;
        data.get_mut(off..end).ok_or(DeviceError::FAILED)?.copy_from_slice(&buf.0);
        Ok(())
    }

    fn block_count(&self) -> u64 {
        (self.data.borrow().len() / BLOCK_SIZE) as u64
    }
}


/// The bound an image-backed reader applies to a block number the image itself
/// named, at every way of getting it wrong.
///
/// It runs here rather than in a guest because there is nothing architectural
/// about it: a block number, a length, and the comparison between them. The
/// extent list lives *inside* the image, so a corrupt one names a block past
/// its own end and a reader without a length reads whatever follows.
#[cfg(all(test, feature = "std"))]
mod image_bounds {
    use super::*;

    /// Three blocks of image, each byte stamped with its block number.
    fn image(blocks: usize) -> VecBlockIO {
        let mut v = vec![0u8; blocks * BLOCK_SIZE];
        for b in 0..blocks {
            v[b * BLOCK_SIZE..(b + 1) * BLOCK_SIZE].fill(b as u8 + 1);
        }
        VecBlockIO::from_vec(v)
    }

    #[test]
    fn every_block_the_image_holds_is_served_whole() {
        let io = image(3);
        assert_eq!(io.block_count(), 3);
        for b in 0..3u64 {
            let mut buf = BlockBuf::zeroed();
            io.read_block(BlockNum::new(b), &mut buf).expect("block inside the image");
            assert!(buf.0.iter().all(|&x| x == b as u8 + 1), "block {b} served the wrong bytes");
        }
    }

    #[test]
    fn a_block_past_the_end_is_refused_rather_than_read() {
        let io = image(3);
        let mut buf = BlockBuf::zeroed();
        for b in [3u64, 4, u64::MAX / BLOCK_SIZE as u64] {
            assert_eq!(io.read_block(BlockNum::new(b), &mut buf), Err(DeviceError::FAILED));
        }
    }

    /// A block that *starts* inside the image and ends past it. `get(off..end)`
    /// is what makes this a refusal; an `off < len` test would serve a short
    /// read out of whatever follows.
    #[test]
    fn a_block_that_starts_inside_and_ends_outside_is_refused() {
        let io = VecBlockIO::from_vec(vec![7u8; 2 * BLOCK_SIZE + 1]);
        let mut buf = BlockBuf::zeroed();
        io.read_block(BlockNum::new(1), &mut buf).expect("the last whole block");
        assert_eq!(
            io.read_block(BlockNum::new(2), &mut buf),
            Err(DeviceError::FAILED),
            "one byte of a block is not a block"
        );
    }

    /// `block.raw() as usize * BLOCK_SIZE` wraps for a block number above
    /// `usize::MAX / 4096`. The kernel builds this crate with
    /// `overflow-checks = true`, so the unchecked multiply is a kernel panic
    /// from a number an image chose — and without the checks it is a small
    /// in-range offset into somebody else's memory.
    #[test]
    fn a_block_number_whose_byte_offset_wraps_is_refused_and_does_not_panic() {
        let io = image(3);
        let mut buf = BlockBuf::zeroed();
        for n in [
            usize::MAX as u64 / BLOCK_SIZE as u64 + 1,
            u64::MAX,
            1u64 << 52,
            (1u64 << 52) + 1,
        ] {
            assert_eq!(byte_range(BlockNum::new(n)), None, "block {n:#x} named a byte range");
            assert_eq!(io.read_block(BlockNum::new(n), &mut buf), Err(DeviceError::FAILED));
        }
        // The largest block number that does express a range still names one
        // no image holds.
        let last = usize::MAX as u64 / BLOCK_SIZE as u64 - 1;
        assert!(byte_range(BlockNum::new(last)).is_some());
        assert_eq!(io.read_block(BlockNum::new(last), &mut buf), Err(DeviceError::FAILED));
    }

    /// An empty image holds no block, including block 0 — the superblock's.
    #[test]
    fn an_empty_image_holds_no_block_at_all() {
        let io = VecBlockIO::from_vec(Vec::new());
        let mut buf = BlockBuf::zeroed();
        assert_eq!(io.block_count(), 0);
        assert_eq!(io.read_block(BlockNum::new(0), &mut buf), Err(DeviceError::FAILED));
    }
}
