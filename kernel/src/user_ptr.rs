//! Safe user memory access via page table walk + kernel direct map. A copy into
//! user memory lands only where a ring 3 store from the process itself could.
//!
//! SMAP stays enabled; access goes through the direct map, never stac/clac.
//! Values are copied out, never referenced. **Every copy pins every frame it
//! covers** under the address-space lock for the copy's life — [`object_run`]
//! a typed value's, [`window`] a [`UserBytes`]/[`UserBytesMut`] buffer's — so a
//! sibling's `munmap` cannot reissue the backing between the translation and
//! the copy, nor under a copy across a park.
//! A window is the physical runs its pages sit in, and a buffer's copy is cut at
//! their seams; a typed value lies inside one 2 MiB page, so in one run.
//! Every single-word user read is `read_volatile`, including the futex word
//! and the crash dump's walk, both outside this module.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::marker::PhantomData;

use toyos_abi::syscall::SyscallError;
use toyos_userbound::{Pinned, Pins, Segment};

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

// SAFETY: `#[repr(C)] Copy`, fourteen `u64`s, no padding; every field is validated where it is used, not here.
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
    space.leaf(addr, access).map(|(phys, _)| crate::mm::DirectMap::from_phys(phys))
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
fn object<T: UserSafe>(ptr: UserAddr, access: Access) -> Result<(*mut T, FramePins<[Segment; 1]>), SyscallError> {
    let (kptr, pins) = object_run(ptr, core::mem::size_of::<T>(), core::mem::align_of::<T>(), access)?;
    Ok((kptr.cast(), pins))
}

/// The one run a typed value lies in, pinned, with nothing allocated.
fn object_run(ptr: UserAddr, size: usize, align: usize, access: Access) -> Result<(*mut u8, FramePins<[Segment; 1]>), SyscallError> {
    if !toyos_userbound::is_user_object(ptr.raw(), size as u64, align as u64) {
        return Err(SyscallError::BadAddress);
    }
    let pins = pinned(ptr, size, access, |_| [Segment { phys: 0, len: 0 }], |one, i, run| one[i] = run)
        .ok_or(SyscallError::BadAddress)?;
    let phys = pins.runs()[0].phys;
    Ok((crate::mm::DirectMap::from_phys(phys).as_mut_ptr(), pins))
}

/// A 2 MiB window is one frame in order, so it holds at most one run.
#[cold]
#[inline(never)]
fn window_split(ptr: UserAddr, len: usize) -> ! {
    panic!("[{:#x}, +{len:#x}) lies in more physical runs than it touches 2 MiB windows", ptr.raw())
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
        Some(UserBytes(View::window(ptr, len, Access::Read)?))
    }

    /// A bulk buffer the kernel writes into and never borrows.
    pub fn user_bytes_mut(&self, ptr: UserAddr, len: u64) -> Option<UserBytesMut<'a>> {
        Some(UserBytesMut(View::window(ptr, len, Access::Write)?))
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
        let (kptr, _pins) = object::<T>(ptr, Access::Read)?;
        // SAFETY: `object::<T>` validated size/align inside one run and pinned its frames until `_pins` drops; `T: UserSafe` makes every bit pattern valid; `read_volatile` guards against a concurrent write from another thread of the same process.
        Ok(unsafe { kptr.read_volatile() })
    }

    /// Write a typed value into user memory.
    pub fn copy_out<T: UserSafe>(&self, ptr: UserAddr, value: &T) -> Result<(), SyscallError> {
        let (kptr, _pins) = object::<T>(ptr, Access::Write)?;
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

/// Where a window's bytes are: the runs it pinned itself, or its parent's for a
/// [`sub`](UserBytes::sub) view, which borrows the parent's pins through the
/// returned lifetime.
enum Runs<'a> {
    Pinned(FramePins<Vec<Segment>>),
    Borrowed(&'a [Segment]),
}

/// `len` bytes starting `off` bytes into `runs`: the one shape both window types are.
struct View<'a> {
    runs: Runs<'a>,
    off: usize,
    len: usize,
}

impl View<'_> {
    fn window(ptr: UserAddr, len: u64, access: Access) -> Option<Self> {
        let len = len as usize;
        let runs = match len {
            0 => Runs::Borrowed(&[]),
            _ => Runs::Pinned(window(ptr, len, access)?),
        };
        Some(View { runs, off: 0, len })
    }

    fn runs(&self) -> &[Segment] {
        match &self.runs {
            Runs::Pinned(pins) => pins.runs(),
            Runs::Borrowed(runs) => runs,
        }
    }

    /// Hands `copy` each piece of bytes `[off, off + len)` of this view as a
    /// direct-map pointer and the piece's offset in that range; panics if out of
    /// bounds — the kernel's own arithmetic, never user input.
    fn pieces(&self, what: &str, off: usize, len: usize, mut copy: impl FnMut(*mut u8, usize, usize)) {
        assert!(
            off.checked_add(len).is_some_and(|end| end <= self.len),
            "{what} {off}+{len} past a {}-byte window",
            self.len
        );
        toyos_userbound::pieces(self.runs(), (self.off + off) as u64, len as u64, |phys, at, n| {
            copy(crate::mm::DirectMap::from_phys(phys).as_mut_ptr(), at as usize, n as usize)
        });
    }

    fn sub(&self, what: &str, off: usize, len: usize) -> View<'_> {
        assert!(
            off.checked_add(len).is_some_and(|end| end <= self.len),
            "{what} {off}+{len} past a {}-byte window",
            self.len
        );
        View { runs: Runs::Borrowed(self.runs()), off: self.off + off, len }
    }
}

/// A bulk buffer the kernel copies out of and never borrows: no reference exists because another thread of the same process can rewrite the bytes at any time; not volatile per byte, since the missing reference already stops the compiler assuming stability.
pub struct UserBytes<'a>(View<'a>);

impl UserBytes<'_> {
    pub fn len(&self) -> usize {
        self.0.len
    }

    /// Copy `dst.len()` bytes out of the window at `off`; panics if out of bounds — the kernel's own arithmetic, never user input.
    pub fn read_at(&self, off: usize, dst: &mut [u8]) {
        let into = dst.as_mut_ptr();
        self.0.pieces("UserBytes::read_at", off, dst.len(), |run, at, n| {
            // SAFETY: `pieces` bounds `[at, at + n)` inside `dst` and `run..+n` inside one run `window` pinned; `dst` is an owned `&mut`, so the ranges cannot overlap.
            unsafe { core::ptr::copy_nonoverlapping(run, into.add(at), n) }
        });
    }

    /// Copy the window at `off` into one ring run; panics if out of bounds, as
    /// [`read_at`](Self::read_at) does. `copy` for [`UserBytesMut::write_run`]'s
    /// reason.
    pub fn read_run(&self, off: usize, dst: &mut toyos_abi::ring::Dst<'_>) {
        let into = dst.as_mut_ptr();
        self.0.pieces("UserBytes::read_run", off, dst.len(), |run, at, n| {
            // SAFETY: as `UserBytesMut::write_run`, mirrored.
            unsafe { core::ptr::copy(run, into.add(at), n) }
        });
    }

    /// The `len`-byte window at `off` inside this one.
    pub fn sub(&self, off: usize, len: usize) -> UserBytes<'_> {
        UserBytes(self.0.sub("UserBytes::sub", off, len))
    }
}

/// A bulk buffer the kernel copies into and never reads back, so it cannot act on a value another thread substituted.
pub struct UserBytesMut<'a>(View<'a>);

impl UserBytesMut<'_> {
    pub fn len(&self) -> usize {
        self.0.len
    }

    pub fn is_empty(&self) -> bool {
        self.0.len == 0
    }

    /// Copy `src` into the window at `off`; panics if out of bounds, for the same reason as [`UserBytes::read_at`].
    pub fn write_at(&mut self, off: usize, src: &[u8]) {
        let from = src.as_ptr();
        self.0.pieces("UserBytesMut::write_at", off, src.len(), |run, at, n| {
            // SAFETY: `pieces` bounds `[at, at + n)` inside `src` and `run..+n` inside one run `window` pinned and proved writable.
            unsafe { core::ptr::copy_nonoverlapping(from.add(at), run, n) }
        });
    }

    /// Copy one ring run into the window at `off`; panics if out of bounds, as
    /// [`write_at`](Self::write_at) does. **`copy` and not
    /// `copy_nonoverlapping`**: `SYS_PIPE_MAP` maps the ring's page into the
    /// caller, which may then name it as this window.
    pub fn write_run(&mut self, off: usize, src: &toyos_abi::ring::Src<'_>) {
        let from = src.as_ptr();
        self.0.pieces("UserBytesMut::write_run", off, src.len(), |run, at, n| {
            // SAFETY: `pieces` bounds `[at, at + n)` inside the ring run, which `Ring::read` proved is inside its own data region, and `run..+n` inside one run `window` pinned and proved writable. Neither side is a reference, so nothing here claims either range is exclusive.
            unsafe { core::ptr::copy(from.add(at), run, n) }
        });
    }

    /// Zero `len` bytes of the window at `off`.
    pub fn fill_zero(&mut self, off: usize, len: usize) {
        self.0.pieces("UserBytesMut::fill_zero", off, len, |run, _, n| {
            // SAFETY: as `write_at`, with a constant zero byte instead of a slice.
            unsafe { core::ptr::write_bytes(run, 0, n) }
        });
    }

    /// The `len`-byte window at `off` inside this one.
    pub fn sub(&mut self, off: usize, len: usize) -> UserBytesMut<'_> {
        UserBytesMut(self.0.sub("UserBytesMut::sub", off, len))
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

/// A pin on every physical run a user-copy window covers: the PMM reissues none of their frames while it lives, so the window's direct-map pointers stay backed even after a sibling unmaps and frees the range across a park.
type FramePins<R> = Pinned<R, Pmm>;

/// The frame allocator's pins.
struct Pmm;

impl Pins for Pmm {
    fn pin(&mut self, run: Segment) -> bool {
        crate::mm::pmm::pin_range(run.phys, run.len as usize)
    }

    fn unpin(&mut self, run: Segment) {
        crate::mm::pmm::unpin_range(run.phys, run.len as usize);
    }
}

/// Faults every 2 MiB window `[ptr, ptr + len)` touches in, and answers how
/// many that is; what they map is confirmed under the lock afterwards.
fn fault_in(ptr: UserAddr, len: usize, access: Access) -> Option<usize> {
    translate_user(ptr, access)?;
    let end = ptr.raw() + len as u64;
    let mut windows = 1;
    let mut boundary = (ptr.raw() & !(crate::mm::PAGE_2M - 1)) + crate::mm::PAGE_2M;
    while boundary < end {
        translate_user(UserAddr::new(boundary), access)?;
        boundary += crate::mm::PAGE_2M;
        windows += 1;
    }
    Some(windows)
}

/// A bulk buffer's runs, one per 2 MiB window at most, allocated before the lock.
fn window(ptr: UserAddr, len: usize, access: Access) -> Option<FramePins<Vec<Segment>>> {
    if !toyos_userbound::in_user_half(ptr.raw(), len as u64) {
        return None;
    }
    pinned(ptr, len, access, Vec::with_capacity, |runs, _, run| runs.push(run))
}

/// Faults `[ptr, ptr+len)` in, walks it leaf by leaf into its physical runs, and pins every frame they cover. `runs` makes the store for as many runs as the range touches 2 MiB windows, and `place` puts run `i` in it. The pins are taken under the address-space lock over a translation that still names each frame, so a concurrent `munmap` — which needs that same lock to free the range — cannot reissue a frame between the confirmation and the pin. Nothing is allocated or freed under the lock but on a refused pin.
fn pinned<R: AsRef<[Segment]>>(
    ptr: UserAddr,
    len: usize,
    access: Access,
    runs: impl FnOnce(usize) -> R,
    mut place: impl FnMut(&mut R, usize, Segment),
) -> Option<FramePins<R>> {
    let windows = fault_in(ptr, len, access)?;
    let mut runs = runs(windows);
    let mut placed = 0;
    let pt = crate::process::current_address_space();
    let guard = pt.lock();
    toyos_userbound::segments(
        ptr.raw(),
        len as u64,
        |at| guard.leaf(UserAddr::new(at), access),
        |run| {
            if placed == windows {
                window_split(ptr, len)
            }
            place(&mut runs, placed, run);
            placed += 1;
        },
    )?;
    let pins = FramePins::pin(runs, Pmm)?;
    drop(guard);
    Some(pins)
}

