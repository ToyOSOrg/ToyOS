//! The volume's bytes, over a [`BlockDev`]'s blocks.
//!
//! **This crate reasons in BPB sectors and reaches the device in bytes**: a
//! sector is whatever the boot sector says, usually 512, and parsing it is
//! what the first read is for. So a request here is any byte range, and it
//! goes to the device as one request for the whole blocks it touches, a part
//! block read before it is written. Nothing is kept between requests: a cache,
//! where a volume wants one, is a device under this.
//!
//! The volume starts at the device's first byte, and every offset is bounded
//! by the geometry before it is asked for.

use alloc::vec;
use alloc::vec::Vec;

use toyos_blockdev::{Block, BlockDev, Error, BLOCK};

/// The blocks a byte range lies on: the first, how many, and where in the
/// first the range begins.
fn blocks_of(offset: u64, len: usize) -> Result<(u64, usize, usize), Error> {
    let end = offset.checked_add(len as u64).ok_or(Error::Range)?;
    let first = offset / BLOCK as u64;
    let count = usize::try_from(end.div_ceil(BLOCK as u64) - first).map_err(|_| Error::Range)?;
    Ok((first, count, (offset % BLOCK as u64) as usize))
}

pub(crate) trait Volume {
    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), Error>;
    async fn write_at(&self, offset: u64, buf: &[u8]) -> Result<(), Error>;
    /// [`read_at`](Self::read_at) of a file's data ([`BlockDev::read_data`]).
    async fn read_data_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), Error>;
    /// [`write_at`](Self::write_at) of a file's data ([`BlockDev::write_data`]).
    async fn write_data_at(&self, offset: u64, buf: &[u8]) -> Result<(), Error>;
}

impl<D: BlockDev> Volume for D {
    async fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), Error> {
        read(self, offset, buf, false).await
    }

    async fn write_at(&self, offset: u64, buf: &[u8]) -> Result<(), Error> {
        write(self, offset, buf, false).await
    }

    async fn read_data_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), Error> {
        read(self, offset, buf, true).await
    }

    async fn write_data_at(&self, offset: u64, buf: &[u8]) -> Result<(), Error> {
        write(self, offset, buf, true).await
    }
}

async fn blocks_in<D: BlockDev>(dev: &D, first: u64, out: &mut [Block], data: bool) -> Result<(), Error> {
    if data { dev.read_data(first, out).await } else { dev.read(first, out).await }
}

async fn read<D: BlockDev>(dev: &D, offset: u64, buf: &mut [u8], data: bool) -> Result<(), Error> {
    let (first, count, at) = blocks_of(offset, buf.len())?;
    if at == 0 && buf.len().is_multiple_of(BLOCK) {
        return blocks_in(dev, first, buf.as_chunks_mut::<BLOCK>().0, data).await;
    }
    let mut span: Vec<Block> = vec![[0; BLOCK]; count];
    blocks_in(dev, first, &mut span, data).await?;
    buf.copy_from_slice(&span.as_flattened()[at..at + buf.len()]);
    Ok(())
}

async fn write<D: BlockDev>(dev: &D, offset: u64, buf: &[u8], data: bool) -> Result<(), Error> {
    let (first, count, at) = blocks_of(offset, buf.len())?;
    let put = async |lba: u64, blocks: &[Block]| if data { dev.write_data(lba, blocks).await } else { dev.write(lba, blocks).await };
    if at == 0 && buf.len().is_multiple_of(BLOCK) {
        return put(first, buf.as_chunks::<BLOCK>().0).await;
    }
    let mut span: Vec<Block> = vec![[0; BLOCK]; count];
    // What the range leaves of its first and last blocks is another
    // structure's, read before it is written back.
    let last = count - 1;
    if at != 0 {
        blocks_in(dev, first, &mut span[..1], data).await?;
    }
    if !(at + buf.len()).is_multiple_of(BLOCK) && !(last == 0 && at != 0) {
        blocks_in(dev, first + last as u64, &mut span[last..], data).await?;
    }
    span.as_flattened_mut()[at..at + buf.len()].copy_from_slice(buf);
    put(first, &span).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::Cell;
    use toyos_blockdev::{block_on_ready, Bytes};

    /// Four blocks holding `0x5A`, the second of which refuses every read.
    struct Refusing {
        bytes: Bytes<Vec<u8>>,
        refused: Cell<u64>,
    }

    impl BlockDev for Refusing {
        fn blocks(&self) -> u64 {
            self.bytes.blocks()
        }
        async fn read(&self, lba: u64, out: &mut [Block]) -> Result<(), Error> {
            if (lba..lba + out.len() as u64).contains(&self.refused.get()) {
                return Err(Error::Device);
            }
            self.bytes.read(lba, out).await
        }
        async fn write(&self, lba: u64, data: &[Block]) -> Result<(), Error> {
            self.bytes.write(lba, data).await
        }
        async fn flush(&self) -> Result<(), Error> {
            Ok(())
        }
    }

    fn refusing() -> Refusing {
        Refusing { bytes: Bytes::new(vec![0x5A; 4 * BLOCK]), refused: Cell::new(1) }
    }

    /// A read the device refused is the device's word to the caller, never a
    /// buffer of zeros: a caller that took zeros for data merges its bytes
    /// into them and writes the result over what the medium held.
    #[test]
    fn a_refused_read_is_an_error_and_never_zeros() {
        let dev = refusing();
        let mut buf = [0xEE; 100];
        assert_eq!(block_on_ready(dev.read_at(BLOCK as u64 + 10, &mut buf)), Err(Error::Device));
        let mut fine = [0; 100];
        block_on_ready(dev.read_at(10, &mut fine)).unwrap();
        assert!(fine.iter().all(|&x| x == 0x5A), "the readable block reads as written");
    }

    /// A write that covers part of a block reads the rest of it first; when
    /// that read is refused the write is refused whole, and nothing reaches
    /// the block — the rest of it is another structure's.
    #[test]
    fn a_partial_write_over_an_unreadable_block_writes_nothing() {
        let dev = refusing();
        assert_eq!(block_on_ready(dev.write_at(BLOCK as u64 + 10, &[1, 2, 3])), Err(Error::Device));
        assert!(dev.bytes.get().iter().all(|&x| x == 0x5A), "the unreadable block was written over");
    }

    /// A part block at either end of a range keeps the bytes the range does
    /// not cover.
    #[test]
    fn a_write_across_part_blocks_keeps_what_it_does_not_cover() {
        let dev = Bytes::new(vec![0x5A; 3 * BLOCK]);
        block_on_ready(dev.write_at(BLOCK as u64 - 2, &[1; BLOCK + 4])).unwrap();
        let held = dev.get();
        assert!(held[..BLOCK - 2].iter().all(|&x| x == 0x5A));
        assert!(held[BLOCK - 2..2 * BLOCK + 2].iter().all(|&x| x == 1));
        assert!(held[2 * BLOCK + 2..].iter().all(|&x| x == 0x5A));
    }
}
