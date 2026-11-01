use core::cell::{Ref, RefCell};

use crate::{within, Block, BlockDev, Error, BLOCK};

/// A device over memory, which answers every request as it is asked: an image
/// being built, or one read whole. A flush has nothing to do.
pub struct Bytes<B> {
    bytes: RefCell<B>,
    blocks: u64,
}

impl<B: AsRef<[u8]> + AsMut<[u8]>> Bytes<B> {
    /// `bytes` is whole blocks.
    pub fn new(bytes: B) -> Self {
        let len = bytes.as_ref().len();
        assert!(len.is_multiple_of(BLOCK), "a device over {len} bytes, which are no whole blocks");
        Self { blocks: (len / BLOCK) as u64, bytes: RefCell::new(bytes) }
    }

    /// What the device holds now.
    pub fn get(&self) -> Ref<'_, B> {
        self.bytes.borrow()
    }

    pub fn into_inner(self) -> B {
        self.bytes.into_inner()
    }

    fn at(&self, lba: u64, count: usize) -> Result<usize, Error> {
        within(lba, count, self.blocks)?;
        Ok(lba as usize * BLOCK)
    }
}

impl<B: AsRef<[u8]> + AsMut<[u8]>> BlockDev for Bytes<B> {
    fn blocks(&self) -> u64 {
        self.blocks
    }

    async fn read(&self, lba: u64, out: &mut [Block]) -> Result<(), Error> {
        let at = self.at(lba, out.len())?;
        let out = out.as_flattened_mut();
        out.copy_from_slice(&self.bytes.borrow().as_ref()[at..at + out.len()]);
        Ok(())
    }

    async fn write(&self, lba: u64, data: &[Block]) -> Result<(), Error> {
        let at = self.at(lba, data.len())?;
        let data = data.as_flattened();
        self.bytes.borrow_mut().as_mut()[at..at + data.len()].copy_from_slice(data);
        Ok(())
    }

    async fn flush(&self) -> Result<(), Error> {
        Ok(())
    }
}

/// A device over memory its holder only reads, such as a signed image held
/// in place: every write is refused [`Error::ReadOnly`].
pub struct ReadOnlyBytes<'a> {
    bytes: &'a [u8],
}

impl<'a> ReadOnlyBytes<'a> {
    /// `None` unless `bytes` is whole blocks.
    pub fn new(bytes: &'a [u8]) -> Option<Self> {
        bytes.len().is_multiple_of(BLOCK).then_some(Self { bytes })
    }
}

impl BlockDev for ReadOnlyBytes<'_> {
    fn blocks(&self) -> u64 {
        (self.bytes.len() / BLOCK) as u64
    }

    async fn read(&self, lba: u64, out: &mut [Block]) -> Result<(), Error> {
        within(lba, out.len(), self.blocks())?;
        let at = lba as usize * BLOCK;
        let out = out.as_flattened_mut();
        out.copy_from_slice(&self.bytes[at..at + out.len()]);
        Ok(())
    }

    async fn write(&self, _: u64, _: &[Block]) -> Result<(), Error> {
        Err(Error::ReadOnly)
    }

    async fn flush(&self) -> Result<(), Error> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_on_ready;
    use alloc::vec;

    #[test]
    fn blocks_read_back_and_a_request_past_the_end_is_refused() {
        let dev = Bytes::new(vec![0u8; 4 * BLOCK]);
        block_on_ready(async {
            dev.write(1, &[[7; BLOCK], [8; BLOCK]]).await.unwrap();
            let mut out = [[0; BLOCK]; 3];
            dev.read(0, &mut out).await.unwrap();
            assert_eq!(out, [[0; BLOCK], [7; BLOCK], [8; BLOCK]]);
            assert_eq!(dev.write(3, &[[1; BLOCK], [1; BLOCK]]).await, Err(Error::Range));
            assert_eq!(dev.read(u64::MAX, &mut out).await, Err(Error::Range));
        });
        assert!(dev.get()[3 * BLOCK..].iter().all(|&b| b == 0), "a refused write wrote");
    }

    #[test]
    fn read_only_bytes_read_back_and_refuse_writes_and_part_blocks() {
        let bytes: alloc::vec::Vec<u8> = (0..2 * BLOCK).map(|i| (i / BLOCK) as u8 + 1).collect();
        assert!(ReadOnlyBytes::new(&bytes[..BLOCK + 512]).is_none());
        let dev = ReadOnlyBytes::new(&bytes).unwrap();
        block_on_ready(async {
            let mut out = [[0; BLOCK]; 1];
            dev.read(1, &mut out).await.unwrap();
            assert_eq!(out, [[2; BLOCK]]);
            assert_eq!(dev.read(2, &mut out).await, Err(Error::Range));
            assert_eq!(dev.read(u64::MAX, &mut out).await, Err(Error::Range));
            assert_eq!(dev.write(0, &out).await, Err(Error::ReadOnly));
        });
    }

    #[test]
    #[should_panic(expected = "no whole blocks")]
    fn a_device_over_part_of_a_block_is_refused() {
        Bytes::new(vec![0u8; BLOCK + 512]);
    }
}
