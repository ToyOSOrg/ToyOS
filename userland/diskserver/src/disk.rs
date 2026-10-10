//! A partition as a program that reads and writes its blocks holds it: one a
//! block service serves ([`Served`]), or one the kernel's own disk serves
//! through a claim ([`Claimed`]), until usbd serves the stick.
//!
//! **A block is 4 KiB here, on every source.** A partition that is not whole
//! 4 KiB blocks is refused before it is served (diskserver's table read, the
//! kernel's claim), so nothing below has a second block size.
//!
//! **A block service that ends is survived, not hidden**: a [`Served`] disk
//! whose session ended reconnects through the same name and issues again every
//! write no flush had covered (`toyos_blockring::client`), then asks what it
//! was asking. What was on the wire when the session ended is asked again
//! because a block write names its whole destination and means the same thing
//! twice.

use crate::{Error as SessionError, Outcome, Session};
use toyos::PartitionDev;
use toyos_abi::part::{Block, MAX_BLOCKS_PER_CALL};
use toyos_blockring::MAX_REQUEST_BLOCKS;

pub const BLOCK: usize = 4096;

/// Why a disk did not do what it was asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiskError {
    /// The device's own word: the transfer was attempted and did not happen,
    /// or a flush left writes the device would not keep.
    Device,
    /// The block service is gone and would not come back.
    Gone,
    /// Past the end of the partition.
    Range,
}

/// A partition, a block at a time or several.
pub trait Disk {
    fn blocks(&self) -> u64;
    /// `out.len()` is whole blocks.
    fn read(&mut self, first: u64, out: &mut [u8]) -> Result<(), DiskError>;
    /// `data.len()` is whole blocks; durable once [`Disk::flush`] answers `Ok`.
    fn write(&mut self, first: u64, data: &[u8]) -> Result<(), DiskError>;
    /// Every write answered before this, out of the device's cache.
    fn flush(&mut self) -> Result<(), DiskError>;
}

/// How many blocks `len` bytes from `first` are, or `Range` past the end of
/// a partition `blocks` long.
pub fn span(first: u64, len: usize, blocks: u64) -> Result<u64, DiskError> {
    assert!(len % BLOCK == 0, "a transfer of {len} bytes is no whole blocks");
    let count = (len / BLOCK) as u64;
    match first.checked_add(count) {
        Some(end) if end <= blocks => Ok(count),
        _ => Err(DiskError::Range),
    }
}

/// A partition the kernel's own disk serves: the stick, until usbd serves it.
pub struct Claimed {
    claim: PartitionDev,
    blocks: u64,
}

impl Claimed {
    pub fn new(claim: PartitionDev) -> Result<Self, DiskError> {
        let info = claim.describe().map_err(|_| DiskError::Device)?;
        Ok(Self { claim, blocks: info.blocks })
    }
}

impl Disk for Claimed {
    fn blocks(&self) -> u64 {
        self.blocks
    }

    fn read(&mut self, first: u64, out: &mut [u8]) -> Result<(), DiskError> {
        span(first, out.len(), self.blocks)?;
        let mut buf: Vec<Block> = vec![[0u8; BLOCK]; MAX_BLOCKS_PER_CALL];
        for (i, chunk) in out.chunks_mut(BLOCK * MAX_BLOCKS_PER_CALL).enumerate() {
            let n = chunk.len() / BLOCK;
            let at = first + (i * MAX_BLOCKS_PER_CALL) as u64;
            self.claim.read(at, &mut buf[..n]).map_err(|_| DiskError::Device)?;
            for (dst, src) in chunk.chunks_exact_mut(BLOCK).zip(&buf[..n]) {
                dst.copy_from_slice(src);
            }
        }
        Ok(())
    }

    fn write(&mut self, first: u64, data: &[u8]) -> Result<(), DiskError> {
        span(first, data.len(), self.blocks)?;
        let mut buf: Vec<Block> = vec![[0u8; BLOCK]; MAX_BLOCKS_PER_CALL];
        for (i, chunk) in data.chunks(BLOCK * MAX_BLOCKS_PER_CALL).enumerate() {
            let n = chunk.len() / BLOCK;
            for (dst, src) in buf[..n].iter_mut().zip(chunk.chunks_exact(BLOCK)) {
                dst.copy_from_slice(src);
            }
            let at = first + (i * MAX_BLOCKS_PER_CALL) as u64;
            self.claim.write(at, &buf[..n]).map_err(|_| DiskError::Device)?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DiskError> {
        self.claim.sync().map_err(|_| DiskError::Device)
    }
}

/// A partition diskserver serves.
pub struct Served {
    session: Session,
    blocks: u64,
}

/// How many times one request is asked again across block service restarts
/// before the disk is called gone: the supervisor gives up on a service that ends
/// faster than this.
const MAX_RECONNECTS: u32 = 4;

impl Served {
    pub fn new(session: Session) -> Self {
        let blocks = session.blocks();
        Self { session, blocks }
    }

    /// Ask, and across a session's end, reconnect and ask again.
    fn asked<T>(&mut self, mut ask: impl FnMut(&mut Session) -> Result<T, SessionError>) -> Result<T, DiskError> {
        let mut reconnects = 0;
        loop {
            match ask(&mut self.session) {
                Ok(answer) => return Ok(answer),
                Err(SessionError::Ended) if reconnects < MAX_RECONNECTS => {
                    reconnects += 1;
                    println!("the block service ended; reconnecting ({reconnects} of {MAX_RECONNECTS})");
                    if let Err(why) = self.session.reconnect() {
                        println!("the block service would not take the session back: {why:?}");
                        return Err(DiskError::Gone);
                    }
                }
                Err(_) => return Err(DiskError::Gone),
            }
        }
    }
}

impl Disk for Served {
    fn blocks(&self) -> u64 {
        self.blocks
    }

    fn read(&mut self, first: u64, out: &mut [u8]) -> Result<(), DiskError> {
        span(first, out.len(), self.blocks)?;
        let per = MAX_REQUEST_BLOCKS as usize;
        for (i, chunk) in out.chunks_mut(BLOCK * per).enumerate() {
            let n = (chunk.len() / BLOCK) as u32;
            let at = first + (i * per) as u64;
            let (outcome, data) = self.asked(|s| s.read(at, n))?;
            match (outcome, data) {
                (Outcome::Done, Some(data)) if data.len() == chunk.len() => chunk.copy_from_slice(&data),
                (Outcome::Refused, _) => return Err(DiskError::Gone),
                _ => return Err(DiskError::Device),
            }
        }
        Ok(())
    }

    fn write(&mut self, first: u64, data: &[u8]) -> Result<(), DiskError> {
        span(first, data.len(), self.blocks)?;
        let per = MAX_REQUEST_BLOCKS as usize;
        for (i, chunk) in data.chunks(BLOCK * per).enumerate() {
            let at = first + (i * per) as u64;
            match self.asked(|s| s.write(at, chunk))? {
                Outcome::Done => {}
                // On the wire when the session ended: the write names its whole
                // destination, so it is written again rather than guessed at.
                Outcome::Refused => match self.asked(|s| s.write(at, chunk))? {
                    Outcome::Done => {}
                    _ => return Err(DiskError::Device),
                },
                _ => return Err(DiskError::Device),
            }
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), DiskError> {
        for _ in 0..=MAX_RECONNECTS {
            match self.asked(Session::flush)? {
                Outcome::Durable => return Ok(()),
                // The session ended under it: the client issues its covered
                // writes again first on the new one, then the flush is asked.
                Outcome::Refused => continue,
                _ => return Err(DiskError::Device),
            }
        }
        Err(DiskError::Gone)
    }
}
