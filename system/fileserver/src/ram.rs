//! The blocks memory stands in for a partition with.

use std::cell::RefCell;
use std::collections::BTreeMap;

use toyos_blockdev::{within, Block, BlockDev, Error};

use crate::cache::Disk;

/// A volume in memory: the DATA role on a machine with no DATA partition, as
/// the kernel's tmpfs was. Sparse, so what nothing wrote costs nothing.
pub struct Ram {
    blocks: u64,
    written: RefCell<BTreeMap<u64, Box<Block>>>,
}

impl Ram {
    pub fn new(blocks: u64) -> Self {
        Self { blocks, written: RefCell::default() }
    }
}

impl BlockDev for Ram {
    fn blocks(&self) -> u64 {
        self.blocks
    }

    async fn read(&self, lba: u64, out: &mut [Block]) -> Result<(), Error> {
        within(lba, out.len(), self.blocks)?;
        let written = self.written.borrow();
        for (block, out) in (lba..).zip(out) {
            match written.get(&block) {
                Some(data) => *out = **data,
                None => out.fill(0),
            }
        }
        Ok(())
    }

    async fn write(&self, lba: u64, data: &[Block]) -> Result<(), Error> {
        within(lba, data.len(), self.blocks)?;
        let mut written = self.written.borrow_mut();
        for (block, data) in (lba..).zip(data) {
            written.insert(block, Box::new(*data));
        }
        Ok(())
    }

    async fn flush(&self) -> Result<(), Error> {
        Ok(())
    }
}

impl Disk for Ram {}
