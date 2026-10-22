//! A partition with every write held until [`Cached`]'s flush writes the
//! blocks it touched in runs, then flushes the partition. A held block is read
//! from here and never from the partition.
//!
//! **Held, because FAT writes a cluster at a time**, and a slot volume's
//! clusters are smaller than a block: `toyos-fat32` reads a block before it
//! writes part of it, so written through, a kernel image would be a read and a
//! write of one block per 512 bytes. Held, the volume's writes are as many
//! requests as the blocks it touched need, and its reads only of the blocks it
//! has not written.

use std::cell::RefCell;
use std::collections::BTreeMap;

use toyos_blockdev::{within, Block, BlockDev, Error};

/// The longest run one write to the partition carries, in blocks.
const RUN: usize = 32;

pub struct Cached<'a, D> {
    disk: &'a D,
    /// Every block written since the last flush, by block number.
    dirty: RefCell<BTreeMap<u64, Box<Block>>>,
}

impl<'a, D: BlockDev> Cached<'a, D> {
    pub fn new(disk: &'a D) -> Self {
        Self { disk, dirty: RefCell::default() }
    }
}

impl<D: BlockDev> BlockDev for Cached<'_, D> {
    fn blocks(&self) -> u64 {
        self.disk.blocks()
    }

    /// Each held block from here, and each run of the others from the
    /// partition.
    async fn read(&self, lba: u64, out: &mut [Block]) -> Result<(), Error> {
        within(lba, out.len(), self.disk.blocks())?;
        let mut unheld = Vec::new();
        {
            let dirty = self.dirty.borrow();
            let mut run: Option<(usize, usize)> = None;
            for (k, out) in out.iter_mut().enumerate() {
                match dirty.get(&(lba + k as u64)) {
                    Some(held) => {
                        *out = **held;
                        unheld.extend(run.take());
                    }
                    None => run = Some(run.map_or((k, k + 1), |(from, _)| (from, k + 1))),
                }
            }
            unheld.extend(run);
        }
        for (from, to) in unheld {
            self.disk.read(lba + from as u64, &mut out[from..to]).await?;
        }
        Ok(())
    }

    async fn write(&self, lba: u64, data: &[Block]) -> Result<(), Error> {
        within(lba, data.len(), self.disk.blocks())?;
        let mut dirty = self.dirty.borrow_mut();
        for (block, data) in (lba..).zip(data) {
            dirty.insert(block, Box::new(*data));
        }
        Ok(())
    }

    /// Every held block written, a run of consecutive ones per request, then
    /// the partition flushed. A block whose write was refused is no longer
    /// held: the install that asked is refused.
    async fn flush(&self) -> Result<(), Error> {
        let held = std::mem::take(&mut *self.dirty.borrow_mut());
        let mut run: Vec<Block> = Vec::with_capacity(RUN);
        let mut first = 0;
        for (block, data) in held {
            if !run.is_empty() && (block != first + run.len() as u64 || run.len() == RUN) {
                self.disk.write(first, &run).await?;
                run.clear();
            }
            if run.is_empty() {
                first = block;
            }
            run.push(*data);
        }
        if !run.is_empty() {
            self.disk.write(first, &run).await?;
        }
        self.disk.flush().await
    }
}
