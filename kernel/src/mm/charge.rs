//! The kernel heap a process causes, charged to its job before it is
//! allocated and given back when what it covers goes.
//!
//! **An object that outlives the call that made it holds a [`Charge`] for
//! every heap allocation it keeps**, taken in its constructor before the
//! allocations and dropped with it, against the job that made it: the
//! caller's ([`current`]), or the one a spawn makes it for. A charge is
//! refused, `ResourceExhausted` at the call, where covering it would take
//! what the frame allocator holds back for the kernel or another job's floor
//! (`pmm::charge`); an allocation it covers then finds room, since the heap
//! grows into what charges reserved first, as long as the heap's
//! fragmentation stays within what the charges' estimates overcount
//! (`issues/the-kernel-heaps-fragmentation-is-charged-to-nobody.md`). What a
//! call allocates and frees before it returns, past what its thread is
//! charged for, is not charged: the kernel's kept superframes cover it.

use alloc::sync::Arc;
use core::alloc::Layout;

use kernel::frames::PoolSplit;
use kernel::sched::job::Job;
use toyos_abi::syscall::SyscallError;

use super::pmm;

/// Covering a charge would take what is held back.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Exhausted;

impl From<Exhausted> for SyscallError {
    fn from(Exhausted: Exhausted) -> Self {
        SyscallError::ResourceExhausted
    }
}

/// The job the running thread is in, whose account what its call causes is
/// charged to; before any thread runs, the boot's own.
pub fn current() -> Arc<Job> {
    crate::sched::driver::current_job().unwrap_or_else(|| Arc::clone(boot()))
}

/// The account of what the kernel makes before any thread runs, made by
/// [`init`].
static BOOT: core::sync::atomic::AtomicPtr<Arc<Job>> = core::sync::atomic::AtomicPtr::new(core::ptr::null_mut());

/// Make the boot's account; `mm::init` calls it once the heap is up.
pub(super) fn init() {
    let job: &'static Arc<Job> = alloc::boxed::Box::leak(alloc::boxed::Box::new(crate::object::job::new_job()));
    BOOT.store(core::ptr::from_ref(job).cast_mut(), core::sync::atomic::Ordering::Release);
}

pub fn boot() -> &'static Arc<Job> {
    let job = BOOT.load(core::sync::atomic::Ordering::Acquire);
    assert!(!job.is_null(), "charge: a charge before `mm::init`");
    // SAFETY: `init` stored a leaked box's pointer, never cleared or freed.
    unsafe { &*job }
}

/// Bytes of the kernel heap charged to a job, given back when it drops. Each
/// growth comes from the pool the job charges at the time (`frames::PoolSplit`).
pub struct Charge {
    job: Arc<Job>,
    held: PoolSplit,
}

impl core::fmt::Debug for Charge {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:?} of job {}", self.held, self.job.key().0)
    }
}

impl Charge {
    /// No bytes yet, charged to `job` as it grows.
    pub fn none(job: &Arc<Job>) -> Self {
        Self { job: Arc::clone(job), held: PoolSplit::NONE }
    }

    /// `bytes` of heap, charged to `job`.
    pub fn new(job: &Arc<Job>, bytes: usize) -> Result<Self, Exhausted> {
        let mut charge = Self::none(job);
        charge.grow(bytes)?;
        Ok(charge)
    }

    /// `bytes` of heap, charged to the caller's job.
    pub fn current(bytes: usize) -> Result<Self, Exhausted> {
        Self::new(&current(), bytes)
    }

    /// `more` bytes besides, refused whole.
    pub fn grow(&mut self, more: usize) -> Result<(), Exhausted> {
        if more == 0 || pmm::charge(&self.job, &mut self.held, more as u64) {
            Ok(())
        } else {
            Err(Exhausted)
        }
    }

    /// `less` bytes fewer.
    pub fn shrink(&mut self, less: usize) {
        if less > 0 {
            pmm::uncharge(&self.job, &mut self.held, less as u64);
        }
    }

    /// Hold exactly `bytes`: grown, refused whole, or shrunk.
    pub fn resize(&mut self, bytes: usize) -> Result<(), Exhausted> {
        let held = self.held.bytes();
        match (bytes as u64).checked_sub(held) {
            Some(more) => self.grow(more as usize),
            None => {
                self.shrink((held - bytes as u64) as usize);
                Ok(())
            }
        }
    }

    pub fn bytes(&self) -> usize {
        self.held.bytes() as usize
    }

    /// The job it is charged to.
    pub fn job(&self) -> &Arc<Job> {
        &self.job
    }
}

impl Drop for Charge {
    fn drop(&mut self) {
        let bytes = self.held.bytes();
        if bytes > 0 {
            pmm::uncharge(&self.job, &mut self.held, bytes);
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
