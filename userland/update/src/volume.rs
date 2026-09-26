//! A partition claim as `toyos-fat32` reads one: byte-addressed, over 4 KiB
//! blocks, with every write held until [`Cached::flush`] writes the blocks it
//! touched in runs of whole calls.
//!
//! **Held, because FAT writes a cluster at a time**, and a slot volume's
//! clusters are smaller than a block: written through, a kernel image would be
//! a read and a write of one block per 512 bytes. Held, the volume's writes are
//! as many calls as the blocks it touched need, and the partition's own fsync
//! is what makes them durable — which is the caller's, after this.

use std::collections::BTreeMap;

use toyos::PartitionDev;
use toyos_abi::part::{Block, BLOCK_BYTES, MAX_BLOCKS_PER_CALL};
use toyos_fat32::{BlockAccess, IoError};

pub struct Cached<'a> {
    claim: &'a PartitionDev,
    blocks: u64,
    /// Every block written since the last flush, by block number.
    dirty: BTreeMap<u64, Box<Block>>,
}

impl<'a> Cached<'a> {
    pub fn new(claim: &'a PartitionDev, blocks: u64) -> Self {
        Self { claim, blocks, dirty: BTreeMap::new() }
    }

    /// Block `n` as the volume now reads: the held write, or the device's.
    fn block(&self, n: u64) -> Result<Block, IoError> {
        if let Some(held) = self.dirty.get(&n) {
            return Ok(**held);
        }
        let mut one = [[0u8; BLOCK_BYTES]];
        self.claim.read(n, &mut one).map_err(|_| IoError::Device)?;
        Ok(one[0])
    }

    /// Each block `[offset, offset + len)` touches, with the part of it that
    /// range covers and where in the caller's buffer that part begins.
    fn spans(&self, offset: u64, len: usize) -> Result<Vec<(u64, std::ops::Range<usize>, usize)>, IoError> {
        let end = offset.checked_add(len as u64).ok_or(IoError::Device)?;
        if end > self.capacity() {
            return Err(IoError::Device);
        }
        let mut out = Vec::new();
        let mut at = offset;
        while at < end {
            let n = at / BLOCK_BYTES as u64;
            let from = (at % BLOCK_BYTES as u64) as usize;
            let to = BLOCK_BYTES.min(from + (end - at) as usize);
            out.push((n, from..to, (at - offset) as usize));
            at += (to - from) as u64;
        }
        Ok(out)
    }
}

impl BlockAccess for Cached<'_> {
    fn capacity(&self) -> u64 {
        self.blocks * BLOCK_BYTES as u64
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), IoError> {
        for (n, range, into) in self.spans(offset, buf.len())? {
            let block = self.block(n)?;
            buf[into..into + range.len()].copy_from_slice(&block[range]);
        }
        Ok(())
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> Result<(), IoError> {
        for (n, range, from) in self.spans(offset, buf.len())? {
            // A whole block's write needs nothing read under it.
            let mut block = if range.len() == BLOCK_BYTES { [0u8; BLOCK_BYTES] } else { self.block(n)? };
            block[range.clone()].copy_from_slice(&buf[from..from + range.len()]);
            self.dirty.insert(n, Box::new(block));
        }
        Ok(())
    }

    /// Write every held block, a run of consecutive ones per call. Durable is
    /// the claim's fsync, which the caller asks once every volume is written.
    fn flush(&mut self) -> Result<(), IoError> {
        let held = std::mem::take(&mut self.dirty);
        let mut run: Vec<Block> = Vec::with_capacity(MAX_BLOCKS_PER_CALL);
        let mut first = 0u64;
        for (n, block) in held {
            if !run.is_empty() && (n != first + run.len() as u64 || run.len() == MAX_BLOCKS_PER_CALL) {
                self.claim.write(first, &run).map_err(|_| IoError::Device)?;
                run.clear();
            }
            if run.is_empty() {
                first = n;
            }
            run.push(*block);
        }
        if !run.is_empty() {
            self.claim.write(first, &run).map_err(|_| IoError::Device)?;
        }
        Ok(())
    }
}
