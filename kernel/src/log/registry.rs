//! Registry mapping each AP's cpu id to its published `Shard` pointer.
//! Compiled a second time by `kernel-loom`; may name only what `shard.rs` shims for that crate, and [`CpuSlice`].

#[cfg(not(feature = "loom"))]
use core::sync::atomic::{AtomicPtr, Ordering};
#[cfg(feature = "loom")]
use loom::sync::atomic::{AtomicPtr, Ordering};

use crate::cpu_slice::CpuSlice;

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

/// A slot per AP: cpu0 has none, its shard being the boot shard, reachable
/// without a lookup.
pub type Slots = CpuSlice<AtomicPtr<Shard>>;

#[cfg(not(feature = "loom"))]
static AP_SHARDS: Slots = Slots::new();

/// A slot for each AP of a machine of `cpus` CPUs, none published.
pub fn size(slots: &Slots, cpus: usize) {
    slots.size(cpus.saturating_sub(1), |_| AtomicPtr::new(core::ptr::null_mut()));
}

/// Publishes `shard` as the shard for `cpu`.
/// # Safety
/// `shard` must be a live, initialised [`Shard`] that is never freed.
pub unsafe fn publish(slots: &Slots, cpu: u32, shard: *mut Shard) {
    let slot = (cpu as usize)
        .checked_sub(1)
        .and_then(|ap| slots.get(ap))
        .unwrap_or_else(|| panic!("log: cpu{cpu} has no shard slot among {} APs", slots.len()));
    slot.store(shard, PUBLISH);
}

/// The shard AP `ap` (cpu `ap + 1`) published, or `None` if it has none yet.
pub fn published(slots: &Slots, ap: usize) -> Option<&'static Shard> {
    let ptr = slots.get(ap)?.load(OBSERVE);
    // SAFETY: `publish`'s contract guarantees a live, never-freed shard written once.
    (!ptr.is_null()).then(|| unsafe { &*ptr })
}

/// The kernel's own registry, which is the one `emit`'s readers walk.
#[cfg(not(feature = "loom"))]
pub fn kernel_slots() -> &'static Slots {
    &AP_SHARDS
}
