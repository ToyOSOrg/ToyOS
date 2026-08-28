//! Registry mapping each AP's cpu id to its published `Shard` pointer.
//! Compiled a second time by `kernel-loom`; may name only what `shard.rs` shims for that crate.

#[cfg(not(feature = "loom"))]
use core::sync::atomic::{AtomicPtr, Ordering};
#[cfg(feature = "loom")]
use loom::sync::atomic::{AtomicPtr, Ordering};

use toyos_abi::log::MAX_LOG_SHARDS;

// Orders the shard's zeroing before the pointer; relaxed is loom's negative control only.
#[cfg(not(feature = "shard-publish-relaxed"))]
const PUBLISH: Ordering = Ordering::Release;
#[cfg(feature = "shard-publish-relaxed")]
const PUBLISH: Ordering = Ordering::Relaxed;
#[cfg(not(feature = "shard-publish-relaxed"))]
const OBSERVE: Ordering = Ordering::Acquire;
#[cfg(feature = "shard-publish-relaxed")]
const OBSERVE: Ordering = Ordering::Relaxed;

use super::shard::Shard;

// cpu0 has no slot: its shard is the boot shard, reachable without a lookup.
#[cfg(not(feature = "loom"))]
static AP_SHARDS: [AtomicPtr<Shard>; MAX_LOG_SHARDS - 1] =
    [const { AtomicPtr::new(core::ptr::null_mut()) }; MAX_LOG_SHARDS - 1];

/// Loom's atomics have no `const` constructor; callers build slots and pass them to `publish`/`published` instead of a static.
#[cfg(feature = "loom")]
pub type Slots = [AtomicPtr<Shard>; MAX_LOG_SHARDS - 1];

#[cfg(feature = "loom")]
pub fn slots() -> Slots {
    core::array::from_fn(|_| AtomicPtr::new(core::ptr::null_mut()))
}

/// Publishes `shard` as the shard for `cpu`.
/// # Safety: `shard` must be a live, initialised [`Shard`] that is never freed.
// cpu0 never calls this: `alloc_log_shard` returns before reaching it, so a zero `cpu` is a caller bug, not a valid case.
pub unsafe fn publish(slots: &[AtomicPtr<Shard>], cpu: u32, shard: *mut Shard) {
    let slot = (cpu as usize)
        .checked_sub(1)
        .and_then(|ap| slots.get(ap))
        .unwrap_or_else(|| panic!("log: cpu{cpu} has no shard slot in an ABI of {MAX_LOG_SHARDS}"));
    slot.store(shard, PUBLISH);
}

/// The shard `cpu` published, or `None` if it has none yet.
pub fn published(slots: &[AtomicPtr<Shard>], ap: usize) -> Option<&'static Shard> {
    let ptr = slots.get(ap)?.load(OBSERVE);
    // SAFETY: `publish`'s contract guarantees a live, never-freed shard written once.
    (!ptr.is_null()).then(|| unsafe { &*ptr })
}

/// The kernel's own registry, which is the one `emit`'s readers walk.
#[cfg(not(feature = "loom"))]
pub fn kernel_slots() -> &'static [AtomicPtr<Shard>] {
    &AP_SHARDS
}
