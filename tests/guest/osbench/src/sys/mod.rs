//! What std does not offer portably, as each OS offers it: the cheapest
//! syscall, a futex, a clock every process shares, anonymous mappings and the
//! machine's conditions. Both modules export the same names; Linux adds
//! affinity, which ToyOS has none of, and the readers of its cgroup and
//! autogroup.

#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod imp;

#[cfg(target_os = "toyos")]
#[path = "toyos.rs"]
mod imp;

pub use imp::*;

/// A whole anonymous read-write mapping, unmapped on drop.
pub struct Mapping {
    ptr: *mut u8,
    len: usize,
}

impl Mapping {
    /// A mapping backed by huge pages wherever the system gives them.
    pub fn huge(len: usize) -> Mapping {
        Mapping { ptr: map_anon_huge(len), len }
    }

    pub fn ptr(&self) -> *mut u8 {
        self.ptr
    }

    /// The mapping as words. Anonymous memory reads as zero, so every word is
    /// initialised.
    pub fn words(&mut self) -> &mut [u64] {
        // SAFETY: `ptr` is page-aligned, `len` bytes long and owned by `self`.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.cast::<u64>(), self.len / 8) }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: nothing borrows the mapping past `self`.
        unsafe { unmap(self.ptr, self.len) }
    }
}
