//! A block device as a filesystem awaits it ([`BlockDev`]), the executor that
//! drives those awaits ([`executor`]), devices over memory ([`Bytes`],
//! [`ReadOnlyBytes`]) and, behind the `model` feature, the crash model a
//! filesystem's ordering is checked against ([`model`]).
//!
//! **A block is [`BLOCK`] bytes on every device**: the block service refuses
//! a partition that is not whole 4 KiB blocks before it serves it, so nothing
//! above this has a second block size. What a device keeps whole across a
//! power loss can be smaller, and is the crash model's to say.
//!
//! **Methods take `&self`**: several tasks may have requests on one device at
//! once, and requests in flight together are unordered. A caller that needs
//! one to follow another awaits the first.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

mod bytes;
pub mod executor;
#[cfg(any(test, feature = "model"))]
pub mod model;

pub use bytes::{Bytes, ReadOnlyBytes};
pub use executor::{block_on, block_on_ready, Executor};

use alloc::rc::Rc;

/// The bytes of one block.
pub const BLOCK: usize = 4096;

/// One block.
pub type Block = [u8; BLOCK];

/// A block's bytes shared between whoever wrote them and a device that may
/// write them again: a served disk keeps a write's until a flush covers it.
/// The block is boxed on its own so the file server's cache can allocate a
/// block it reads in fallibly (`system/fileserver/src/cache.rs`, `boxed`),
/// which `Rc::new` of the block itself cannot.
pub type SharedBlock = Rc<alloc::boxed::Box<Block>>;

/// Why a device did not do what it was asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The device's own word: the transfer was attempted and did not happen,
    /// or a flush left writes the device would not keep. A write answered
    /// this may or may not have reached the medium.
    Device,
    /// The device is gone and will not come back.
    Gone,
    /// Past the end of the device.
    Range,
    /// A write this holder's grant does not make.
    ReadOnly,
}

/// A device of [`BlockDev::blocks`] blocks.
// Every future runs on this crate's single-threaded executor, so none needs
// to be `Send`: the bound the lint asks to be stated is one no caller wants.
#[allow(async_fn_in_trait)]
pub trait BlockDev {
    fn blocks(&self) -> u64;

    /// Blocks `lba..lba + out.len()` into `out`.
    async fn read(&self, lba: u64, out: &mut [Block]) -> Result<(), Error>;

    /// `data` at `lba`, complete once the device has taken it. Durable only
    /// once a [`BlockDev::flush`] asked after this completed has completed.
    async fn write(&self, lba: u64, data: &[Block]) -> Result<(), Error>;

    /// Every write that completed before this was asked, on the medium.
    async fn flush(&self) -> Result<(), Error>;

    /// [`read`](Self::read) of a file's data rather than the filesystem's own
    /// structures: a device that caches the structures may pass this by.
    async fn read_data(&self, lba: u64, out: &mut [Block]) -> Result<(), Error> {
        self.read(lba, out).await
    }

    /// [`write`](Self::write) of a file's data, as [`read_data`](Self::read_data).
    async fn write_data(&self, lba: u64, data: &[Block]) -> Result<(), Error> {
        self.write(lba, data).await
    }
}

/// `Ok` if `count` blocks from `lba` lie on a device of `blocks` blocks.
pub fn within(lba: u64, count: usize, blocks: u64) -> Result<(), Error> {
    match lba.checked_add(count as u64) {
        Some(end) if end <= blocks => Ok(()),
        _ => Err(Error::Range),
    }
}

impl<D: BlockDev + ?Sized> BlockDev for &D {
    fn blocks(&self) -> u64 {
        (**self).blocks()
    }
    async fn read(&self, lba: u64, out: &mut [Block]) -> Result<(), Error> {
        (**self).read(lba, out).await
    }
    async fn write(&self, lba: u64, data: &[Block]) -> Result<(), Error> {
        (**self).write(lba, data).await
    }
    async fn flush(&self) -> Result<(), Error> {
        (**self).flush().await
    }
    async fn read_data(&self, lba: u64, out: &mut [Block]) -> Result<(), Error> {
        (**self).read_data(lba, out).await
    }
    async fn write_data(&self, lba: u64, data: &[Block]) -> Result<(), Error> {
        (**self).write_data(lba, data).await
    }
}

impl<D: BlockDev + ?Sized> BlockDev for Rc<D> {
    fn blocks(&self) -> u64 {
        (**self).blocks()
    }
    async fn read(&self, lba: u64, out: &mut [Block]) -> Result<(), Error> {
        (**self).read(lba, out).await
    }
    async fn write(&self, lba: u64, data: &[Block]) -> Result<(), Error> {
        (**self).write(lba, data).await
    }
    async fn flush(&self) -> Result<(), Error> {
        (**self).flush().await
    }
    async fn read_data(&self, lba: u64, out: &mut [Block]) -> Result<(), Error> {
        (**self).read_data(lba, out).await
    }
    async fn write_data(&self, lba: u64, data: &[Block]) -> Result<(), Error> {
        (**self).write_data(lba, data).await
    }
}
