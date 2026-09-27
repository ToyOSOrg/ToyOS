//! Safe user memory access via page table walk + kernel direct map. A copy into
//! user memory lands only where a ring 3 store from the process itself could.
//!
//! SMAP stays enabled; access goes through the direct map, never stac/clac.
//! Values are copied out, never referenced. **Every copy goes through
//! [`window`]**, which pins every frame it covers under the address-space lock
//! for the copy's life — a typed value's as much as a [`UserBytes`]/
//! [`UserBytesMut`] buffer's — so a sibling's `munmap` cannot reissue the
//! backing between the translation and the copy, nor under a copy across a park.
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

pub(crate) use toyos_userbound::Access;

fn translate_now(
    space: &crate::mm::paging::AddressSpace,
    addr: UserAddr,
    access: Access,
) -> Option<crate::mm::DirectMap> {
    match access {
        Access::Read => space.translate(addr),
        Access::Write => space.translate_writable(addr),
    }
}

/// Translate a user virtual address to its direct-map address, demand-paging it in if needed; `pub(crate)` because the futex word outlives its syscall.
pub(crate) fn translate_user(addr: UserAddr, access: Access) -> Option<crate::mm::DirectMap> {
    let pt = crate::process::current_address_space();
    if let Some(dm) = translate_now(&pt.lock(), addr, access) {
        return Some(dm);
    }
    if !crate::process::handle_page_fault(addr.raw(), 0) {
        return None;
    }
    let result = translate_now(&pt.lock(), addr, access);
    result
}

/// A `T` at `ptr` as a pinned window; `is_user_object` keeps it aligned and
/// inside one 2 MiB page.
fn object<T: UserSafe>(ptr: UserAddr, access: Access) -> Result<(*mut T, FramePin), SyscallError> {
    let size = core::mem::size_of::<T>();
    if !toyos_userbound::is_user_object(ptr.raw(), size as u64, core::mem::align_of::<T>() as u64) {
        return Err(SyscallError::BadAddress);
    }
    let (kptr, pin) = window(ptr, size, access).ok_or(SyscallError::BadAddress)?;
    Ok((kptr.cast(), pin))
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
        let (kptr, pin) = window(ptr, len, Access::Read)?;
        Some(UserBytes { kptr: kptr as *const u8, len, _pin: Some(pin), _scope: PhantomData })
    }

    /// A bulk buffer the kernel writes into and never borrows.
    pub fn user_bytes_mut(&self, ptr: UserAddr, len: u64) -> Option<UserBytesMut<'a>> {
        let len = len as usize;
        if len == 0 {
            let kptr = core::ptr::NonNull::<u8>::dangling().as_ptr();
            return Some(UserBytesMut { kptr, len, _pin: None, _scope: PhantomData });
        }
        let (kptr, pin) = window(ptr, len, Access::Write)?;
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
        let (kptr, _pin) = object::<T>(ptr, Access::Read)?;
        // SAFETY: `object::<T>` validated size/align inside one page and pinned the frame until `_pin` drops; `T: UserSafe` makes every bit pattern valid; `read_volatile` guards against a concurrent write from another thread of the same process.
        Ok(unsafe { kptr.read_volatile() })
    }

    /// Write a typed value into user memory.
    pub fn copy_out<T: UserSafe>(&self, ptr: UserAddr, value: &T) -> Result<(), SyscallError> {
        let (kptr, _pin) = object::<T>(ptr, Access::Write)?;
        if crate::actuator::copy_meets_a_remap() {
            remap_race::hold(kptr.cast(), core::mem::size_of::<T>());
        }
        // SAFETY: as `copy_in`; `T: UserSafe` guarantees no uninitialized padding byte is written out.
        unsafe { kptr.write_volatile(*value) };
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

    /// Copy the window at `off` into one ring run; panics if out of bounds, as
    /// [`read_at`](Self::read_at) does. `copy` for [`UserBytesMut::write_run`]'s
    /// reason.
    pub fn read_run(&self, off: usize, dst: &mut toyos_abi::ring::Dst<'_>) {
        assert!(
            off.checked_add(dst.len()).is_some_and(|end| end <= self.len),
            "UserBytes::read_run {off}+{} past a {}-byte window",
            dst.len(),
            self.len
        );
        // SAFETY: as `UserBytesMut::write_run`, mirrored.
        unsafe {
            core::ptr::copy(self.kptr.add(off), dst.as_mut_ptr(), dst.len());
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

    /// Copy one ring run into the window at `off`; panics if out of bounds, as
    /// [`write_at`](Self::write_at) does. **`copy` and not
    /// `copy_nonoverlapping`**: `SYS_PIPE_MAP` maps the ring's page into the
    /// caller, which may then name it as this window.
    pub fn write_run(&mut self, off: usize, src: &toyos_abi::ring::Src<'_>) {
        assert!(
            off.checked_add(src.len()).is_some_and(|end| end <= self.len),
            "UserBytesMut::write_run {off}+{} past a {}-byte window",
            src.len(),
            self.len
        );
        // SAFETY: the assert proves `off + src.len() <= self.len`, `window` proved the range is one physically contiguous mapping, and `Ring::read` proved the run is inside its own data region. Neither side is a reference, so nothing here claims either range is exclusive.
        unsafe {
            core::ptr::copy(src.as_ptr(), self.kptr.add(off), src.len());
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
fn window(ptr: UserAddr, len: usize, access: Access) -> Option<(*mut u8, FramePin)> {
    if !toyos_userbound::in_user_half(ptr.raw(), len as u64) {
        return None;
    }
    let start = ptr.raw();
    let end = start + len as u64;
    // Fault every page of the range in; what it maps is confirmed under the lock below.
    translate_user(ptr, access)?;
    let mut boundary = (start & !(crate::mm::PAGE_2M - 1)) + crate::mm::PAGE_2M;
    while boundary < end {
        translate_user(UserAddr::new(boundary), access)?;
        boundary += crate::mm::PAGE_2M;
    }
    let pt = crate::process::current_address_space();
    let guard = pt.lock();
    let phys = toyos_userbound::contiguous(start, len as u64, access, |at| {
        translate_now(&guard, UserAddr::new(at), access).map(|dm| dm.phys())
    })?;
    if !crate::mm::pmm::pin_range(phys, len) {
        return None;
    }
    drop(guard);
    Some((crate::mm::DirectMap::from_phys(phys).as_mut_ptr(), FramePin { phys, len }))
}

/// `copy-meets-a-remap`: a sibling's `munmap` and `mmap` staged between a
/// typed copy's translation and its store. Only a copy whose destination
/// already holds [`MARK`] in its first word is held, so the test program
/// chooses the one copy it races.
pub(crate) mod remap_race {
    use core::sync::atomic::{AtomicU64, Ordering};
    use crate::time::Duration;

    /// What the racing program leaves in the destination's first word.
    const MARK: u64 = 0x5eed_c0de_2ace_0001;
    /// What this writes into the second word once the copy is held: the
    /// program's cue to unmap and map again.
    const HELD: u64 = 0x5eed_c0de_2ace_0002;
    /// How long a held copy waits for the program to map again before it says
    /// the test staged nothing.
    const BOUND: Duration = Duration::from_secs(10);

    /// The pid a held copy is waiting on, plus one; zero while none is held.
    static HOLDER: AtomicU64 = AtomicU64::new(0);
    /// Maps the holder's process has completed since its copy was held.
    static MAPS: AtomicU64 = AtomicU64::new(0);

    pub(super) fn hold(kptr: *mut u8, size: usize) {
        if size < 16 {
            return;
        }
        let words = kptr.cast::<u64>();
        // SAFETY: the caller's window covers `size >= 16` bytes at `kptr`, aligned for a `u64`-bearing `UserSafe` type.
        if unsafe { words.read_volatile() } != MARK {
            return;
        }
        let pid = crate::process::current_process().0 as u64 + 1;
        HOLDER.store(pid, Ordering::SeqCst);
        MAPS.store(0, Ordering::SeqCst);
        // SAFETY: as above; the second word is inside the same window.
        unsafe { words.add(1).write_volatile(HELD) };
        let deadline = crate::clock::now() + BOUND;
        while MAPS.load(Ordering::SeqCst) == 0 {
            assert!(
                crate::clock::now() < deadline,
                "copy-meets-a-remap: pid {} held a copy {BOUND} and never mapped again",
                pid - 1
            );
            // An `IF`-clear spin: the sibling's munmap shoots down this CPU too.
            crate::arch::tlb::poll();
            core::hint::spin_loop();
        }
        HOLDER.store(0, Ordering::SeqCst);
    }

    /// `sys_mmap` placed a mapping of its own choosing for the current process.
    pub(crate) fn mapped() {
        if HOLDER.load(Ordering::SeqCst) == crate::process::current_process().0 as u64 + 1 {
            MAPS.fetch_add(1, Ordering::SeqCst);
        }
    }
}
