//! A table with an element per CPU the firmware names: sized once, while the
//! boot CPU runs alone, and never freed, so an interrupt, an NMI or the panic
//! path reads it without allocating. Before it is sized it is empty.
//!
//! One allocation holds the whole table, so an element is a few words: a
//! record of its own per CPU is a `CpuSlice<&'static T>`, each allocated
//! apart.
//!
//! Compiled a second time by `kernel-loom`, so it names nothing of the kernel.

use alloc::boxed::Box;

#[cfg(not(feature = "loom"))]
use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

#[cfg(feature = "loom")]
use loom::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

pub struct CpuSlice<T> {
    /// Published after `len`, so a reader that sees the table sees its length.
    ptr: AtomicPtr<T>,
    len: AtomicUsize,
}

// SAFETY: the elements are shared by reference only, like a `static [T]`.
unsafe impl<T: Sync> Sync for CpuSlice<T> {}

impl<T> CpuSlice<T> {
    /// `const`: the kernel's tables are `static`s.
    #[cfg(not(feature = "loom"))]
    pub const fn new() -> Self {
        Self { ptr: AtomicPtr::new(core::ptr::null_mut()), len: AtomicUsize::new(0) }
    }

    // Not `const`: loom's atomics have no const constructor.
    #[allow(clippy::new_without_default)]
    #[cfg(feature = "loom")]
    pub fn new() -> Self {
        Self { ptr: AtomicPtr::new(core::ptr::null_mut()), len: AtomicUsize::new(0) }
    }

    /// Size the table to `len` elements, the `i`th `init(i)`. Once.
    pub fn size(&self, len: usize, init: impl FnMut(usize) -> T) {
        let table: Box<[T]> = (0..len).map(init).collect();
        self.install(Box::into_raw(table).cast::<T>(), len);
    }

    fn install(&self, ptr: *mut T, len: usize) {
        self.len.store(len, Ordering::Relaxed);
        let was = self.ptr.swap(ptr, Ordering::Release);
        assert!(was.is_null(), "a per-cpu table is sized once");
    }
}

impl<T> core::ops::Deref for CpuSlice<T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        let ptr = self.ptr.load(Ordering::Acquire);
        if ptr.is_null() {
            return &[];
        }
        // SAFETY: `install` published a leaked allocation of `len` elements,
        // its length stored before the pointer this acquire read.
        unsafe { core::slice::from_raw_parts(ptr, self.len.load(Ordering::Relaxed)) }
    }
}

impl<T> Drop for CpuSlice<T> {
    fn drop(&mut self) {
        let ptr = self.ptr.load(Ordering::Acquire);
        if !ptr.is_null() {
            let len = self.len.load(Ordering::Relaxed);
            // SAFETY: the allocation `install` leaked, reached by no reference
            // past this `&mut`.
            drop(unsafe { Box::from_raw(core::ptr::slice_from_raw_parts_mut(ptr, len)) });
        }
    }
}
