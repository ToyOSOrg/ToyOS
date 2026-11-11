//! User memory, copied in and out through the running process's own mapping
//! (`arch::uaccess`): a copy reaches only what a load or store the process
//! made itself could reach, and a byte it cannot reach ends the copy as
//! [`Faulted`], which a syscall answers as `BadAddress`. Values are copied
//! out, never referenced.
//!
//! **A window is a range of the user half**, checked for its end, overflow
//! included, when it is made, and by nothing else: whether its pages are
//! mapped is found by the copy. A page the copy finds absent is served as the
//! process's own fault would be (`process::handle_page_fault`), and the copy
//! resumes; one nothing serves, or one the process may not touch so, ends it.
//! A window over device memory the process mapped is copied through, making
//! only the accesses the process could make through that mapping itself
//! (`arch::uaccess`).
//!
//! **A copy holds no kernel lock**: its fault takes its space's lock, and in
//! 7b will wait on a pager. Every spin lock raises the preemption count, so
//! each copy refuses to run above the depth its syscall's entry runs at
//! ([`gate`]), whether or not a page faults.
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

use crate::arch::uaccess;
use crate::UserAddr;

/// Longest string, in bytes, the kernel accepts from userspace; one bound for every string syscall, since each copies or tokenizes rather than streaming.
pub const MAX_USER_STR: u64 = 64 * 1024;

pub(crate) use toyos_userbound::Access;

fn asked(access: Access) -> Asked {
    match access {
        Access::Read => Asked::Read,
        Access::Write => Asked::Write,
    }
}

/// Translate a user virtual address to its direct-map address, populating its
/// page if needed; `pub(crate)` because the futex word outlives its syscall.
pub(crate) fn translate_user(addr: UserAddr, access: Access) -> Option<crate::mm::DirectMap> {
    let space = unlocked_space();
    let phys = space.lock().reach(addr.raw(), asked(access))?;
    Some(crate::mm::DirectMap::from_phys(phys))
}

/// The current space, for a reach made holding no lock.
fn unlocked_space() -> crate::mm::space::PageTables {
    gate();
    crate::process::current_address_space()
}

/// Refuses a user copy made holding a lock or with interrupts masked, whose
/// fault could not be served: a kernel bug, never a user's doing.
fn gate() {
    let depth = crate::preempt::count();
    if depth > crate::scheduler::BASELINE_TRAP {
        under_a_lock(depth);
    }
    assert!(crate::arch::cpu::interrupts_enabled(), "user_ptr: a user copy with interrupts masked");
}

/// A copy met a user byte its process could not reach.
#[derive(Debug)]
pub struct Faulted;

impl From<Faulted> for SyscallError {
    fn from(_: Faulted) -> Self {
        SyscallError::BadAddress
    }
}

/// The backtrace names the copy's caller.
#[cold]
#[inline(never)]
fn under_a_lock(depth: u32) -> ! {
    panic!("user_ptr: a user copy at preemption depth {depth}, past its syscall's: a lock is held, and its fault-in may wait");
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
        Some(UserBytes(View::window(ptr, len)?, PhantomData))
    }

    /// A bulk buffer the kernel writes into and never borrows.
    pub fn user_bytes_mut(&self, ptr: UserAddr, len: u64) -> Option<UserBytesMut<'a>> {
        Some(UserBytesMut(View::window(ptr, len)?, PhantomData))
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
        let view = object::<T>(ptr)?;
        let mut value = toyos_abi::usersafe::zeroed::<T>();
        view.read_into(0, toyos_abi::usersafe::bytes_mut(&mut value))?;
        Ok(value)
    }

    /// Write a typed value into user memory.
    pub fn copy_out<T: UserSafe>(&self, ptr: UserAddr, value: &T) -> Result<(), SyscallError> {
        let view = object::<T>(ptr)?;
        view.write_from(0, toyos_abi::usersafe::bytes(value))?;
        Ok(())
    }

    /// Copy `len` bytes of user memory onto the kernel heap; every caller bounds `len` itself before calling.
    pub fn user_vec(&self, ptr: UserAddr, len: u64) -> Result<Vec<u8>, SyscallError> {
        let bytes = self.user_bytes(ptr, len).ok_or(SyscallError::BadAddress)?;
        let mut out = vec![0u8; bytes.len()];
        bytes.read_at(0, &mut out)?;
        Ok(out)
    }
}

/// A typed value's window: aligned for `T` and inside the user half.
fn object<T: UserSafe>(ptr: UserAddr) -> Result<View, SyscallError> {
    let (size, align) = (core::mem::size_of::<T>() as u64, core::mem::align_of::<T>() as u64);
    if !toyos_userbound::is_user_object(ptr.raw(), size, align) {
        return Err(SyscallError::BadAddress);
    }
    View::window(ptr, size).ok_or(SyscallError::BadAddress)
}

/// `len` bytes of the user half from `start`.
#[derive(Clone, Copy)]
struct View {
    start: u64,
    len: usize,
}

impl View {
    fn window(ptr: UserAddr, len: u64) -> Option<Self> {
        toyos_userbound::in_user_half(ptr.raw(), len).then_some(Self { start: ptr.raw(), len: usize::try_from(len).ok()? })
    }

    /// The user address of bytes `[off, off + len)` of this view; panics if
    /// out of bounds — the kernel's own arithmetic, never user input.
    fn at(&self, what: &str, off: usize, len: usize) -> u64 {
        assert!(off.checked_add(len).is_some_and(|end| end <= self.len), "{what} {off}+{len} past a {}-byte window", self.len);
        gate();
        self.start + off as u64
    }

    fn read_into(&self, off: usize, dst: &mut [u8]) -> Result<(), Faulted> {
        // SAFETY: `dst` is an owned `&mut` of that length, and `at` keeps the
        // user range inside this window, inside the user half.
        unsafe { self.read_raw(off, dst.as_mut_ptr(), dst.len()) }
    }

    fn write_from(&self, off: usize, src: &[u8]) -> Result<(), Faulted> {
        // SAFETY: as `read_into`, with `src` readable for its length.
        unsafe { self.write_raw(off, src.as_ptr(), src.len()) }
    }

    /// # Safety
    /// `dst` is writable kernel memory for `len` bytes.
    unsafe fn read_raw(&self, off: usize, dst: *mut u8, len: usize) -> Result<(), Faulted> {
        let user = self.at("UserBytes::read", off, len);
        // SAFETY: the caller's `dst`; `at` keeps the user range in the half,
        // and `gate` holds no lock across the copy.
        unsafe { uaccess::copy_from_user(dst, user, len) }
    }

    /// # Safety
    /// `src` is readable kernel memory for `len` bytes.
    unsafe fn write_raw(&self, off: usize, src: *const u8, len: usize) -> Result<(), Faulted> {
        let user = self.at("UserBytesMut::write", off, len);
        // SAFETY: as `read_raw`.
        unsafe { uaccess::copy_to_user(user, src, len) }
    }

    fn fill_zero(&self, off: usize, len: usize) -> Result<(), Faulted> {
        let user = self.at("UserBytesMut::fill_zero", off, len);
        // SAFETY: as `read_raw`, with no kernel memory touched.
        unsafe { uaccess::clear_user(user, len) }
    }
}

/// A bulk buffer the kernel copies out of and never borrows: no reference exists because another thread of the same process can rewrite the bytes at any time; not volatile per byte, since the missing reference already stops the compiler assuming stability.
pub struct UserBytes<'a>(View, PhantomData<&'a ()>);

impl UserBytes<'_> {
    pub fn len(&self) -> usize {
        self.0.len
    }

    /// Copy `dst.len()` bytes out of the window at `off`; panics if out of bounds — the kernel's own arithmetic, never user input.
    pub fn read_at(&self, off: usize, dst: &mut [u8]) -> Result<(), Faulted> {
        self.0.read_into(off, dst)
    }

    /// Copy `len` bytes out of the window at `off` to `dst`, never through a
    /// reference: for memory another CPU may copy into or out of meanwhile.
    ///
    /// # Safety
    /// `dst` is writable for `len` bytes, kernel memory no window names.
    pub unsafe fn read_raw(&self, off: usize, dst: *mut u8, len: usize) -> Result<(), Faulted> {
        // SAFETY: the caller's contract.
        unsafe { self.0.read_raw(off, dst, len) }
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
    pub fn write_at(&mut self, off: usize, src: &[u8]) -> Result<(), Faulted> {
        self.0.write_from(off, src)
    }

    /// Copy `len` bytes from `src` into the window at `off`, never through a
    /// reference: for memory another CPU may write meanwhile.
    ///
    /// # Safety
    /// `src` is readable for `len` bytes, kernel memory no window names.
    pub unsafe fn write_raw(&mut self, off: usize, src: *const u8, len: usize) -> Result<(), Faulted> {
        // SAFETY: the caller's contract.
        unsafe { self.0.write_raw(off, src, len) }
    }

    /// Zero `len` bytes of the window at `off`.
    pub fn fill_zero(&mut self, off: usize, len: usize) -> Result<(), Faulted> {
        self.0.fill_zero(off, len)
    }
}
