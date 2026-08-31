//! Safe user memory access via page table walk + kernel direct map.
//!
//! SMAP stays enabled; access goes through the direct map, never stac/clac.
//! Values are copied out, never referenced; bulk buffers are a [`UserBytes`]/
//! [`UserBytesMut`] window that pins every frame it covers for its own life, so
//! a sibling's `munmap` cannot reissue the backing under a copy across a park.
//! Every single-word user read is `read_volatile`, including the futex word
//! and the crash dump's walk, both outside this module.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::marker::PhantomData;

use toyos_abi::syscall::SyscallError;

use crate::UserAddr;

/// Longest string, in bytes, the kernel accepts from userspace; one bound for every string syscall, since each copies or tokenizes rather than streaming.
pub const MAX_USER_STR: u64 = 64 * 1024;

/// Marker for types safe to interpret from / write to validated user pointers.
/// # Safety
/// Must be `#[repr(C)]`, `Copy`, have no padding, and be valid for any bit pattern.
pub unsafe trait UserSafe: Copy {}

// Every impl below is hand-checked: `#[repr(C)]`, `Copy`, integer fields only, explicit `_pad` for every alignment gap — Rust cannot verify this mechanically. A padding byte would leak kernel stack out through `copy_out` or accept an unwritten value through `copy_in`.

// SAFETY: a primitive integer (and an array of them) is `#[repr(C)]`, has no padding, and every bit pattern is a value.
unsafe impl UserSafe for u32 {}
// SAFETY: see `u32`.
unsafe impl UserSafe for u64 {}
// SAFETY: see `u32` — an array adds no padding between elements.
unsafe impl UserSafe for [u32; 2] {}
// SAFETY: see `u32`.
unsafe impl UserSafe for [u64; 2] {}

// SAFETY: `#[repr(C)] Copy`, three `u64`s, no padding; `file_type` is a `u64`, not the enum it names, so every bit pattern stays valid.
unsafe impl UserSafe for crate::object::ops::Stat {}

// SAFETY: `#[repr(C)] Copy`, ten `u64`s, no padding; every field is validated where it is used, not here.
unsafe impl UserSafe for toyos_abi::syscall::SpawnArgs {}
// SAFETY: `#[repr(C)] Copy`, `RawHandle`, a `flags: u32`, then six `u64`s — no padding.
unsafe impl UserSafe for toyos_abi::syscall::NamespaceBuild {}
// SAFETY: `#[repr(C)] Copy`, `u64`, `u64`, `i64` — 24 bytes, no padding.
unsafe impl UserSafe for toyos_abi::syscall::SchedInfo {}
// SAFETY: `#[repr(C)] Copy`; the two `u32` pairs keep every `u64` 8-aligned, so there is no padding.
unsafe impl UserSafe for toyos_abi::syscall::ProcessStats {}
// SAFETY: `#[repr(C)] Copy`, `[RawHandle; 2]` then six `u32`s — align 4, no padding.
unsafe impl UserSafe for toyos_abi::FramebufferInfo {}
// SAFETY: `#[repr(C)] Copy`, `RawHandle`, an explicit `_pad: u32`, `u64` — no padding.
unsafe impl UserSafe for toyos_abi::syscall::InboxSetup {}

// SAFETY: `#[repr(C)] Copy`, two `u8`s, no padding; `keycode`/`modifiers` are plain `u8`, not enums, so any bit pattern is valid.
unsafe impl UserSafe for toyos_abi::input::RawKeyEvent {}
// SAFETY: `#[repr(C)] Copy`, `u8`, `i8`, `u16`, `u16` — 2-aligned, no padding.
unsafe impl UserSafe for toyos_abi::input::MouseEvent {}

// SAFETY: `#[repr(C)] Copy`, 88 bytes with no padding (checked by a compile-time size assertion); every field is clamped where it is used, not here.
unsafe impl UserSafe for toyos_abi::log::LogCursor {}

/// Translate a user virtual address to its direct-map address, demand-paging it in if needed; `pub(crate)` because the futex word outlives its syscall.
pub(crate) fn translate_user(addr: UserAddr) -> Option<crate::mm::DirectMap> {
    let pt = crate::process::current_address_space();
    if let Some(dm) = pt.lock().translate(addr) {
        return Some(dm);
    }
    if !crate::process::handle_page_fault(addr.raw(), 0) {
        return None;
    }
    let result = pt.lock().translate(addr);
    result
}

fn translate(user_addr: u64) -> Option<*mut u8> {
    translate_user(UserAddr::new(user_addr)).map(|dm| dm.as_mut_ptr())
}

/// The direct-map address of a `T` at `ptr`; `is_user_object` bounds it to one physical page so a copy near the boundary cannot overrun it.
fn object<T: UserSafe>(ptr: UserAddr) -> Result<*mut u8, SyscallError> {
    let ok = toyos_userbound::is_user_object(
        ptr.raw(),
        core::mem::size_of::<T>() as u64,
        core::mem::align_of::<T>() as u64,
    );
    if !ok {
        return Err(SyscallError::BadAddress);
    }
    translate(ptr.raw()).ok_or(SyscallError::BadAddress)
}


/// Context for a single syscall invocation; the lifetime `'a` keeps validated references from escaping it.
pub struct SyscallContext<'a> {
    _scope: PhantomData<&'a mut ()>,
}

impl<'a> SyscallContext<'a> {
    /// # Safety
    /// Caller guarantees the current process's page tables stay active for `'a`.
    pub unsafe fn new() -> Self {
        Self { _scope: PhantomData }
    }

    /// A bulk buffer the kernel reads out of and never borrows.
    pub fn user_bytes(&self, ptr: UserAddr, len: u64) -> Option<UserBytes<'a>> {
        let len = len as usize;
        if len == 0 {
            let kptr = core::ptr::NonNull::<u8>::dangling().as_ptr() as *const u8;
            return Some(UserBytes { kptr, len, _pin: None, _scope: PhantomData });
        }
        let (kptr, pin) = window(ptr, len)?;
        Some(UserBytes { kptr: kptr as *const u8, len, _pin: Some(pin), _scope: PhantomData })
    }

    /// A bulk buffer the kernel writes into and never borrows.
    pub fn user_bytes_mut(&self, ptr: UserAddr, len: u64) -> Option<UserBytesMut<'a>> {
        let len = len as usize;
        if len == 0 {
            let kptr = core::ptr::NonNull::<u8>::dangling().as_ptr();
            return Some(UserBytesMut { kptr, len, _pin: None, _scope: PhantomData });
        }
        let (kptr, pin) = window(ptr, len)?;
        Some(UserBytesMut { kptr, len, _pin: Some(pin), _scope: PhantomData })
    }

    /// Copy a user string of at most [`MAX_USER_STR`] bytes into kernel memory; an over-long or non-UTF-8 string is `InvalidArgument`, not `BadAddress`.
    pub fn user_str(&self, ptr: UserAddr, len: u64) -> Result<String, SyscallError> {
        if len > MAX_USER_STR {
            return Err(SyscallError::InvalidArgument);
        }
        let bytes = self.user_vec(ptr, len)?;
        String::from_utf8(bytes).map_err(|_| SyscallError::InvalidArgument)
    }

    /// Read a typed value out of user memory, copied rather than borrowed.
    pub fn copy_in<T: UserSafe>(&self, ptr: UserAddr) -> Result<T, SyscallError> {
        let kptr = object::<T>(ptr)?;
        // SAFETY: `object::<T>` validated size/align and translated inside one page; `T: UserSafe` makes every bit pattern valid; `read_volatile` guards against a concurrent write from another thread of the same process.
        Ok(unsafe { (kptr as *const T).read_volatile() })
    }

    /// Write a typed value into user memory.
    pub fn copy_out<T: UserSafe>(&self, ptr: UserAddr, value: &T) -> Result<(), SyscallError> {
        let kptr = object::<T>(ptr)?;
        // SAFETY: same validation as `copy_in`; `T: UserSafe` guarantees no uninitialized padding byte is written out.
        unsafe { (kptr as *mut T).write_volatile(*value) };
        Ok(())
    }

    /// Copy `len` bytes of user memory onto the kernel heap; every caller bounds `len` itself before calling.
    pub fn user_vec(&self, ptr: UserAddr, len: u64) -> Result<Vec<u8>, SyscallError> {
        let bytes = self.user_bytes(ptr, len).ok_or(SyscallError::BadAddress)?;
        let mut out = vec![0u8; bytes.len()];
        bytes.read_at(0, &mut out);
        Ok(out)
    }
}

/// A bulk buffer the kernel copies out of and never borrows: no reference exists because another thread of the same process can rewrite the bytes at any time; not volatile per byte, since the missing reference already stops the compiler assuming stability.
pub struct UserBytes<'a> {
    kptr: *const u8,
    len: usize,
    /// Holds the frames covered pinned for this window's life; `None` for the empty window and for a [`sub`](UserBytes::sub) view, which borrows its parent's pin through the returned lifetime.
    _pin: Option<FramePin>,
    _scope: PhantomData<&'a ()>,
}

impl UserBytes<'_> {
    pub fn len(&self) -> usize {
        self.len
    }

    /// Copy `dst.len()` bytes out of the window at `off`; panics if out of bounds — the kernel's own arithmetic, never user input.
    pub fn read_at(&self, off: usize, dst: &mut [u8]) {
        assert!(
            off.checked_add(dst.len()).is_some_and(|end| end <= self.len),
            "UserBytes::read_at {off}+{} past a {}-byte window",
            dst.len(),
            self.len
        );
        // SAFETY: the assert proves `off + dst.len() <= self.len`, and `window` proved the range is one physically contiguous mapping; `dst` is an owned `&mut`, so the ranges cannot overlap.
        unsafe {
            core::ptr::copy_nonoverlapping(self.kptr.add(off), dst.as_mut_ptr(), dst.len());
        }
    }

    /// The `len`-byte window at `off` inside this one.
    pub fn sub(&self, off: usize, len: usize) -> UserBytes<'_> {
        assert!(
            off.checked_add(len).is_some_and(|end| end <= self.len),
            "UserBytes::sub {off}+{len} past a {}-byte window",
            self.len
        );
        // SAFETY: the assert proves `off + len <= self.len`, so the result stays inside the window `window` validated.
        UserBytes { kptr: unsafe { self.kptr.add(off) }, len, _pin: None, _scope: PhantomData }
    }
}

/// A bulk buffer the kernel copies into and never reads back, so it cannot act on a value another thread substituted.
pub struct UserBytesMut<'a> {
    kptr: *mut u8,
    len: usize,
    /// As [`UserBytes::_pin`]: the frames stay pinned for this window's life.
    _pin: Option<FramePin>,
    _scope: PhantomData<&'a mut ()>,
}

impl UserBytesMut<'_> {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Copy `src` into the window at `off`; panics if out of bounds, for the same reason as [`UserBytes::read_at`].
    pub fn write_at(&mut self, off: usize, src: &[u8]) {
        assert!(
            off.checked_add(src.len()).is_some_and(|end| end <= self.len),
            "UserBytesMut::write_at {off}+{} past a {}-byte window",
            src.len(),
            self.len
        );
        // SAFETY: the assert proves `off + src.len() <= self.len`, and `window` proved the range is one physically contiguous mapping.
        unsafe {
            core::ptr::copy_nonoverlapping(src.as_ptr(), self.kptr.add(off), src.len());
        }
    }

    /// Zero `len` bytes of the window at `off`.
    pub fn fill_zero(&mut self, off: usize, len: usize) {
        assert!(
            off.checked_add(len).is_some_and(|end| end <= self.len),
            "UserBytesMut::fill_zero {off}+{len} past a {}-byte window",
            self.len
        );
        // SAFETY: same bound as `write_at`, with a constant zero byte instead of a slice.
        unsafe { core::ptr::write_bytes(self.kptr.add(off), 0, len) };
    }

    /// The `len`-byte window at `off` inside this one.
    pub fn sub(&mut self, off: usize, len: usize) -> UserBytesMut<'_> {
        assert!(
            off.checked_add(len).is_some_and(|end| end <= self.len),
            "UserBytesMut::sub {off}+{len} past a {}-byte window",
            self.len
        );
        // SAFETY: [`UserBytes::sub`]'s argument exactly.
        UserBytesMut { kptr: unsafe { self.kptr.add(off) }, len, _pin: None, _scope: PhantomData }
    }
}

/// Bytes the kernel copies *from*, wherever they live — lets `file_cache::write_page` name the capability it needs instead of a concrete window type.
pub trait ByteSource {
    fn len(&self) -> usize;
    fn read_at(&self, off: usize, dst: &mut [u8]);
}

impl ByteSource for [u8] {
    fn len(&self) -> usize {
        <[u8]>::len(self)
    }

    fn read_at(&self, off: usize, dst: &mut [u8]) {
        dst.copy_from_slice(&self[off..off + dst.len()]);
    }
}

impl ByteSource for UserBytes<'_> {
    fn len(&self) -> usize {
        UserBytes::len(self)
    }

    fn read_at(&self, off: usize, dst: &mut [u8]) {
        UserBytes::read_at(self, off, dst);
    }
}

/// A pin on the physical frames a user-copy window covers: the PMM reissues none of them while it lives, so the window's direct-map pointer stays backed even after a sibling unmaps and frees the range across a park.
struct FramePin {
    phys: u64,
    len: usize,
}

impl Drop for FramePin {
    fn drop(&mut self) {
        crate::mm::pmm::unpin_range(self.phys, self.len);
    }
}

/// Validate `[ptr, ptr+len)` as one physically contiguous user window, pin every frame it covers, and return its direct-map address with the pin. The pin is taken under the address-space lock over a translation that still names the frame, so a concurrent `munmap` — which needs that same lock to free the range — cannot reissue a frame between the confirmation and the pin.
fn window(ptr: UserAddr, len: usize) -> Option<(*mut u8, FramePin)> {
    if !toyos_userbound::in_user_half(ptr.raw(), len as u64) {
        return None;
    }
    let start = ptr.raw();
    let end = start + len as u64;
    // Fault every page of the range in; contiguity is confirmed under the lock below.
    translate(start)?;
    let mut boundary = (start & !(crate::mm::PAGE_2M - 1)) + crate::mm::PAGE_2M;
    while boundary < end {
        translate(boundary)?;
        boundary += crate::mm::PAGE_2M;
    }
    let pt = crate::process::current_address_space();
    let guard = pt.lock();
    let base = guard.translate(ptr)?;
    let phys = base.phys();
    let mut boundary = (start & !(crate::mm::PAGE_2M - 1)) + crate::mm::PAGE_2M;
    while boundary < end {
        if guard.translate(UserAddr::new(boundary))?
            != crate::mm::DirectMap::from_phys(phys + (boundary - start))
        {
            return None;
        }
        boundary += crate::mm::PAGE_2M;
    }
    if len > 1
        && guard.translate(UserAddr::new(end - 1))?
            != crate::mm::DirectMap::from_phys(phys + (len as u64 - 1))
    {
        return None;
    }
    if !crate::mm::pmm::pin_range(phys, len) {
        return None;
    }
    drop(guard);
    Some((base.as_mut_ptr(), FramePin { phys, len }))
}
