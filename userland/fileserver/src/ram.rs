//! The blocks memory stands in for a partition with.

use std::collections::BTreeMap;

use diskserver::disk::{span, Disk, DiskError, BLOCK};

/// A volume in memory: the DATA role on a machine with no DATA partition, as
/// the kernel's tmpfs was. Sparse, so what nothing wrote costs nothing.
pub struct Ram {
    blocks: u64,
    written: BTreeMap<u64, Box<[u8; BLOCK]>>,
}

impl Ram {
    pub fn new(blocks: u64) -> Self {
        Self { blocks, written: BTreeMap::new() }
    }
}

impl Disk for Ram {
    fn blocks(&self) -> u64 {
        self.blocks
    }

    fn read(&mut self, first: u64, out: &mut [u8]) -> Result<(), DiskError> {
        span(first, out.len(), self.blocks)?;
        for (i, chunk) in out.chunks_exact_mut(BLOCK).enumerate() {
            match self.written.get(&(first + i as u64)) {
                Some(block) => chunk.copy_from_slice(&block[..]),
                None => chunk.fill(0),
            }
        }
        Ok(())
    }

    fn write(&mut self, first: u64, data: &[u8]) -> Result<(), DiskError> {
        span(first, data.len(), self.blocks)?;
        for (i, chunk) in data.chunks_exact(BLOCK).enumerate() {
            let mut block = Box::new([0u8; BLOCK]);
            block.copy_from_slice(chunk);
            self.written.insert(first + i as u64, block);
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DiskError> {
        Ok(())
    }
}

