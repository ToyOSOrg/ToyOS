//! The kernel heap a process causes, charged before it is allocated and given
//! back when what it covers goes.
//!
//! **An object that outlives the call that made it holds a [`Charge`] for
//! every heap allocation it keeps**, taken in its constructor before the
//! allocations and dropped with it. A charge is refused, `ResourceExhausted`
//! at the call, where covering it would take what the frame allocator holds
//! back for the kernel (`pmm::charge`); an allocation it covers then finds
//! room, since the heap grows into what charges reserved first, as long as
//! the heap's fragmentation stays within what the charges' estimates
//! overcount (`issues/the-kernel-heaps-fragmentation-is-charged-to-nobody.md`).
//! What a call allocates and frees before it returns, past what its thread is
//! charged for, is not charged: the kernel's kept superframes cover it.

use core::alloc::Layout;

use toyos_abi::syscall::SyscallError;

use super::pmm;

/// Covering a charge would take what the kernel holds back.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Exhausted;

impl From<Exhausted> for SyscallError {
    fn from(Exhausted: Exhausted) -> Self {
        SyscallError::ResourceExhausted
    }
}

/// Bytes of the kernel heap charged, given back when it drops.
#[derive(Debug)]
pub struct Charge(u64);

impl Charge {
    /// No bytes: a charge to grow later.
    pub const NONE: Self = Self(0);

    /// `bytes` of heap.
    pub fn new(bytes: usize) -> Result<Self, Exhausted> {
        let mut charge = Self::NONE;
        charge.grow(bytes)?;
        Ok(charge)
    }

    /// `more` bytes besides, refused whole.
    pub fn grow(&mut self, more: usize) -> Result<(), Exhausted> {
        if more == 0 {
            return Ok(());
        }
        if !pmm::charge(more as u64) {
            return Err(Exhausted);
        }
        self.0 += more as u64;
        Ok(())
    }

    /// `less` bytes fewer.
    pub fn shrink(&mut self, less: usize) {
        let less = less as u64;
        assert!(less <= self.0, "charge: {less} of {} bytes given back", self.0);
        if less > 0 {
            pmm::uncharge(less);
            self.0 -= less;
        }
    }

    /// Hold exactly `bytes`: grown, refused whole, or shrunk.
    pub fn resize(&mut self, bytes: usize) -> Result<(), Exhausted> {
        match (bytes as u64).checked_sub(self.0) {
            Some(more) => self.grow(more as usize),
            None => {
                self.shrink((self.0 - bytes as u64) as usize);
                Ok(())
            }
        }
    }

    pub fn bytes(&self) -> usize {
        self.0 as usize
    }
}

impl Drop for Charge {
    fn drop(&mut self) {
        if self.0 > 0 {
            pmm::uncharge(self.0);
        }
    }
}

/// The heap bytes one allocation of `layout` takes.
pub fn heap(layout: Layout) -> usize {
    super::alloc::chunk(layout)
}

/// The heap bytes of one `Arc<T>`: its two counts beside the value.
pub fn arc<T>() -> usize {
    let (inner, _) = Layout::new::<[usize; 2]>().extend(Layout::new::<T>()).expect("charge: an Arc's layout");
    heap(inner.pad_to_align())
}

/// The heap bytes one entry of a `BTreeMap<K, V>` takes, at most: a node
/// other than the root holds at least 5 of its 11 entries, so an entry's
/// share of its leaf is under three times its own bytes, and the internal
/// nodes over every 6 leaves add less than one more.
pub fn map_entry<K, V>() -> usize {
    3 * (core::mem::size_of::<K>() + core::mem::size_of::<V>()) + 64
}

/// The heap bytes of a `Vec<T>` holding `capacity`; past any heap, more
/// than any charge is given.
pub fn array<T>(capacity: usize) -> usize {
    match Layout::array::<T>(capacity) {
        Ok(layout) if layout.size() == 0 => 0,
        Ok(layout) => heap(layout),
        Err(_) => usize::MAX,
    }
}
