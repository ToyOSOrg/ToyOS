//! What a mapping is for and what memory type it gets, named by concept: each
//! architecture's page tables encode these (x86-64's PAT bits, AArch64's
//! `MAIR` indices), and nothing above them spells a bit.

use super::PAGE_2M;

/// 4 KiB pages in one 2 MiB page.
pub const PAGES_PER_2M: usize = (PAGE_2M / 4096) as usize;

/// What a user mapping may be used for: no variant is both writable and
/// executable, and every variant implies read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Prot {
    /// Read-only: neither writable nor executable.
    Read,
    /// Data: readable and writable, never executable.
    ReadWrite,
    /// Code. Never writable.
    ReadExec,
}

/// What each 4 KiB page of a 2 MiB window may be used for: split because
/// `toyos-ld` can align a window across the end of `.text` and start of `.data`.
pub struct WindowProt([Prot; PAGES_PER_2M]);

impl WindowProt {
    /// A window whose pages all say the same thing.
    pub const fn uniform(prot: Prot) -> Self {
        Self([prot; PAGES_PER_2M])
    }

    /// Sets the 4 KiB page `offset` bytes in; an out-of-window offset panics.
    pub fn set(&mut self, offset: u64, prot: Prot) {
        self.0[(offset / 4096) as usize] = prot;
    }

    /// The one protection every page carries, or `None` where they disagree.
    pub(crate) fn agreed(&self) -> Option<Prot> {
        let first = self.0[0];
        self.0.iter().all(|&p| p == first).then_some(first)
    }

    /// Every 4 KiB page's protection, in order.
    pub(crate) fn pages(&self) -> impl Iterator<Item = Prot> + '_ {
        self.0.iter().copied()
    }
}

/// The memory type a mapping gets, out of the three this kernel ever writes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CachePolicy {
    /// Ordinary memory: RAM, write-back.
    Normal,
    /// Uncached and unbuffered, whatever the firmware set or forgot: device
    /// registers.
    Uncacheable,
    /// Uncached with stores gathered: a scanout.
    WriteCombining,
}

/// What an MMIO window may select — never [`CachePolicy::Normal`]: a device
/// register mapped cacheable is one a speculative or combined access can
/// reach, and this type removes that as a possibility.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MmioPolicy {
    /// Registers.
    Uncacheable,
    /// The scanout alone.
    WriteCombining,
}

impl MmioPolicy {
    pub fn cache(self) -> CachePolicy {
        match self {
            Self::Uncacheable => CachePolicy::Uncacheable,
            Self::WriteCombining => CachePolicy::WriteCombining,
        }
    }
}
