//! What a mapping is for and what memory type it gets, named by concept: each
//! architecture's page tables encode these (x86-64's PAT bits, AArch64's
//! `MAIR` indices), and nothing above them spells a bit.

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
