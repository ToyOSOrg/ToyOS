//! Safe user memory access through the kernel direct map. A copy into user
//! memory lands only where a ring 3 store from the process itself could.
//!
//! SMAP stays enabled; access goes through the direct map, never stac/clac.
//! Values are copied out, never referenced.
//!
//! **A window is a range of the caller's own space**, checked against the
//! space's ranges when it is made. Each copy reaches its pages then, a chunk at
//! a time: under the space's lock the chunk's pages are reached — populated
//! where empty — and their frames pinned, and the copy runs with the lock given
//! back and preemption off. So a frame a sibling's `munmap` frees meanwhile is
//! not reissued until the copy is done with it, no pin outlives its chunk, and
//! no CPU holds more than one chunk's pins. A page the process took away since
//! the window was made reads as zeros and drops what is written to it.
//!
//! **A copy is never made under its space's lock**, which its fault-in takes:
//! every entry here refuses one by name ([`crate::mm::space::Space::held_here`]) rather than spin
//! on itself, whether or not the page is populated yet.
//!
//! Every single-word user read is `read_volatile`, including the futex word
//! and the crash dump's walk, both outside this module.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::marker::PhantomData;

use kernel::vm::Access as Asked;
use toyos_abi::syscall::SyscallError;
use toyos_abi::UserSafe;

use crate::mm::PAGE_SIZE;
use crate::UserAddr;

/// Longest string, in bytes, the kernel accepts from userspace; one bound for every string syscall, since each copies or tokenizes rather than streaming.
pub const MAX_USER_STR: u64 = 64 * 1024;

pub(crate) use toyos_userbound::Access;

/// Pages one chunk of a copy reaches and pins at once.
const CHUNK_PAGES: usize = 16;

fn asked(access: Access) -> Asked {
    match access {
        Access::Read => Asked::Read,
        Access::Write => Asked::Write,
    }
}

/// Translate a user virtual address to its direct-map address, populating its
/// page if needed; `pub(crate)` because the futex word outlives its syscall.
pub(crate) fn translate_user(addr: UserAddr, access: Access) -> Option<crate::mm::DirectMap> {
    let space = unheld_space();
    let phys = space.lock().reach(addr.raw(), asked(access))?;
    Some(crate::mm::DirectMap::from_phys(phys))
}

/// The current space, refused where this CPU already holds its lock.
fn unheld_space() -> crate::mm::space::PageTables {
    let space = crate::process::current_address_space();
    if space.held_here() {
        under_its_lock();
    }
    space
}

/// The backtrace names the copy's caller.
#[cold]
#[inline(never)]
fn under_its_lock() -> ! {
    panic!("user_ptr: a user copy under its own space's lock: its fault-in would wait on itself");
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
        Some(UserBytes(View::window(ptr, len, Access::Read)?, PhantomData))
    }

    /// A bulk buffer the kernel writes into and never borrows.
    pub fn user_bytes_mut(&self, ptr: UserAddr, len: u64) -> Option<UserBytesMut<'a>> {
        Some(UserBytesMut(View::window(ptr, len, Access::Write)?, PhantomData))
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
        let view = object::<T>(ptr, Access::Read)?;
        let mut value = toyos_abi::usersafe::zeroed::<T>();
        view.read_into(0, toyos_abi::usersafe::bytes_mut(&mut value));
        Ok(value)
    }

    /// Write a typed value into user memory.
    pub fn copy_out<T: UserSafe>(&self, ptr: UserAddr, value: &T) -> Result<(), SyscallError> {
        let view = object::<T>(ptr, Access::Write)?;
        view.write_from(0, toyos_abi::usersafe::bytes(value));
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

/// A typed value's window: aligned for `T` and inside the user half.
fn object<T: UserSafe>(ptr: UserAddr, access: Access) -> Result<View, SyscallError> {
    let (size, align) = (core::mem::size_of::<T>() as u64, core::mem::align_of::<T>() as u64);
    if !toyos_userbound::is_user_object(ptr.raw(), size, align) {
        return Err(SyscallError::BadAddress);
    }
    View::window(ptr, size, access).ok_or(SyscallError::BadAddress)
}

/// `len` bytes of the caller's space from `start`, which its ranges allowed
/// `access` to when the view was made.
#[derive(Clone, Copy)]
struct View {
    start: u64,
    len: usize,
    access: Access,
}

/// What a page a sibling took away reads as.
static ZEROS: [u8; PAGE_SIZE as usize] = [0; PAGE_SIZE as usize];

impl View {
    fn window(ptr: UserAddr, len: u64, access: Access) -> Option<Self> {
        if !toyos_userbound::in_user_half(ptr.raw(), len) {
            return None;
        }
        let view = Self { start: ptr.raw(), len: usize::try_from(len).ok()?, access };
        if len == 0 {
            return Some(view);
        }
        let space = unheld_space();
        let allowed = space.lock().allows(ptr.raw(), ptr.raw() + len, asked(access));
        allowed.then_some(view)
    }

    /// Hands `copy` each piece of bytes `[off, off + len)` of this view as a
    /// direct-map pointer, the piece's offset in that range and its length;
    /// panics if out of bounds — the kernel's own arithmetic, never user input.
    fn pieces(&self, what: &str, off: usize, len: usize, mut copy: impl FnMut(*mut u8, usize, usize)) {
        assert!(off.checked_add(len).is_some_and(|end| end <= self.len), "{what} {off}+{len} past a {}-byte window", self.len);
        let space = unheld_space();
        let mut done = 0;
        while done < len {
            let mut frames = [None::<u64>; CHUNK_PAGES];
            let mut pieces = [(0u64, 0usize, 0usize); CHUNK_PAGES];
            let mut count = 0;
            {
                let mut guard = space.lock();
                while count < CHUNK_PAGES && done < len {
                    let at = self.start + (off + done) as u64;
                    let n = ((PAGE_SIZE - at % PAGE_SIZE) as usize).min(len - done);
                    let phys = guard
                        .reach(at, asked(self.access))
                        .filter(|&phys| crate::mm::pmm::pin_range(phys & !(PAGE_SIZE - 1), PAGE_SIZE as usize));
                    frames[count] = phys;
                    pieces[count] = (phys.unwrap_or(0), done, n);
                    count += 1;
                    done += n;
                }
            }
            crate::sched::driver::preempt_off(|_| {
                for (frame, &(phys, at, n)) in frames[..count].iter().zip(&pieces[..count]) {
                    match (frame, self.access) {
                        (Some(_), _) => copy(crate::mm::DirectMap::from_phys(phys).as_mut_ptr(), at, n),
                        (None, Access::Read) => copy(ZEROS.as_ptr().cast_mut(), at, n),
                        (None, Access::Write) => {}
                    }
                }
            });
            for phys in frames[..count].iter().flatten() {
                crate::mm::pmm::unpin_range(phys & !(PAGE_SIZE - 1), PAGE_SIZE as usize);
            }
        }
    }

    fn sub(&self, what: &str, off: usize, len: usize) -> View {
        assert!(off.checked_add(len).is_some_and(|end| end <= self.len), "{what} {off}+{len} past a {}-byte window", self.len);
        View { start: self.start + off as u64, len, access: self.access }
    }

    fn read_into(&self, off: usize, dst: &mut [u8]) {
        let into = dst.as_mut_ptr();
        self.pieces("UserBytes::read_at", off, dst.len(), |run, at, n| {
            // SAFETY: `pieces` bounds `[at, at + n)` inside `dst` and `run..+n`
            // inside one pinned page or the zero page; `dst` is an owned
            // `&mut`, so the ranges cannot overlap.
            unsafe { core::ptr::copy_nonoverlapping(run, into.add(at), n) }
        });
    }

    fn write_from(&self, off: usize, src: &[u8]) {
        let from = src.as_ptr();
        self.pieces("UserBytesMut::write_at", off, src.len(), |run, at, n| {
            // SAFETY: `pieces` bounds `[at, at + n)` inside `src` and `run..+n`
            // inside one pinned page its range let the process write.
            unsafe { core::ptr::copy_nonoverlapping(from.add(at), run, n) }
        });
    }
}

/// A bulk buffer the kernel copies out of and never borrows: no reference exists because another thread of the same process can rewrite the bytes at any time; not volatile per byte, since the missing reference already stops the compiler assuming stability.
pub struct UserBytes<'a>(View, PhantomData<&'a ()>);

impl UserBytes<'_> {
    pub fn len(&self) -> usize {
        self.0.len
    }

    /// Copy `dst.len()` bytes out of the window at `off`; panics if out of bounds — the kernel's own arithmetic, never user input.
    pub fn read_at(&self, off: usize, dst: &mut [u8]) {
        self.0.read_into(off, dst);
    }

    /// Hand `copy` each piece of the window's `[off, off + len)`, as
    /// [`read_at`](Self::read_at) reaches them, one chunk of pages pinned at
    /// a time: a pointer to its bytes, its offset in that range and its
    /// length. The pointer is valid for that length until `copy` returns,
    /// with preemption off, and is never made a reference; a page the
    /// process took away reads as zeros. Panics if out of bounds, as
    /// [`read_at`](Self::read_at) does.
    pub fn read_pieces(&self, off: usize, len: usize, mut copy: impl FnMut(*const u8, usize, usize)) {
        self.0.pieces("UserBytes::read_pieces", off, len, |run, at, n| copy(run.cast_const(), at, n));
    }

    /// Copy `len` bytes out of the window at `off` to `dst`, never through a
    /// reference: for memory another CPU may copy into or out of meanwhile.
    ///
    /// # Safety
    /// `dst` is writable for `len` bytes, kernel memory no window names.
    pub unsafe fn read_raw(&self, off: usize, dst: *mut u8, len: usize) {
        self.0.pieces("UserBytes::read_raw", off, len, |run, at, n| {
            // SAFETY: `pieces` bounds `[at, at + n)` inside `len` and `run..+n`
            // inside one pinned page or the zero page; `dst` is the caller's.
            unsafe { core::ptr::copy_nonoverlapping(run, dst.add(at), n) }
        });
    }

    /// The `len`-byte window at `off` inside this one.
    pub fn sub(&self, off: usize, len: usize) -> UserBytes<'_> {
        UserBytes(self.0.sub("UserBytes::sub", off, len), PhantomData)
    }
}

/// A bulk buffer the kernel copies into and never reads back, so it cannot act on a value another thread substituted.
pub struct UserBytesMut<'a>(View, PhantomData<&'a mut ()>);

impl UserBytesMut<'_> {
    pub fn len(&self) -> usize {
        self.0.len
    }

    pub fn is_empty(&self) -> bool {
        self.0.len == 0
    }

    /// Copy `src` into the window at `off`; panics if out of bounds, for the same reason as [`UserBytes::read_at`].
    pub fn write_at(&mut self, off: usize, src: &[u8]) {
        self.0.write_from(off, src);
    }

    /// [`UserBytes::read_pieces`] for a copy into the window: each pointer
    /// may be written for its length until `copy` returns, and a page the
    /// process took away drops what is written to it.
    pub fn write_pieces(&mut self, off: usize, len: usize, copy: impl FnMut(*mut u8, usize, usize)) {
        self.0.pieces("UserBytesMut::write_pieces", off, len, copy);
    }

    /// Copy `len` bytes from `src` into the window at `off`, never through a
    /// reference: for memory another CPU may write meanwhile.
    ///
    /// # Safety
    /// `src` is readable for `len` bytes, kernel memory no window names.
    pub unsafe fn write_raw(&mut self, off: usize, src: *const u8, len: usize) {
        self.0.pieces("UserBytesMut::write_raw", off, len, |run, at, n| {
            // SAFETY: `pieces` bounds `[at, at + n)` inside `len` and `run..+n`
            // inside one pinned page the process may write; `src` is the caller's.
            unsafe { core::ptr::copy_nonoverlapping(src.add(at), run, n) }
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
        UserBytesMut(self.0.sub("UserBytesMut::sub", off, len), PhantomData)
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
