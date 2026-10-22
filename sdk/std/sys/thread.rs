use toyos_abi::syscall;

use crate::cell::UnsafeCell;
use crate::ffi::CStr;
use crate::io;
use crate::mem::ManuallyDrop;
use crate::num::NonZero;
use crate::ptr;
use crate::sync::atomic::{AtomicPtr, AtomicU32, Ordering};
use crate::thread::ThreadInit;
use crate::time::Duration;

/// A thread's handle. Joined, it frees the thread's stack; dropped, it
/// detaches the thread, whose stack the next spawn frees once it has ended.
pub struct Thread {
    block: *mut Block,
}

unsafe impl Send for Thread {}
unsafe impl Sync for Thread {}

/// What a thread and its handle share. Freed by whoever reaps the thread:
/// its joiner, or for a detached thread the first spawn after it ended.
struct Block {
    tid: AtomicU32,
    state: AtomicU32,
    /// The stack's reservation: [`GUARD`] reserved, then the stack committed.
    reservation: *mut u8,
    reserved: usize,
    /// Taken by the thread as it starts.
    init: UnsafeCell<Option<Box<ThreadInit>>>,
    /// The next block on [`REAPED`].
    next: *mut Block,
}

const JOINABLE: u32 = 0;
const DETACHED: u32 = 1;
const EXITED: u32 = 2;

/// Detached threads that have ended, linked through [`Block::next`]: a thread
/// cannot free the stack it stands on, and pushing allocates nothing.
static REAPED: AtomicPtr<Block> = AtomicPtr::new(ptr::null_mut());

pub const DEFAULT_MIN_STACK_SIZE: usize = 2 * 1024 * 1024;

/// Reserved pages below a thread's stack, so that running off it faults: one
/// page is enough, since every frame larger than a page probes each page it
/// spans (the target's inline stack probes).
const GUARD: usize = syscall::PAGE_SIZE;

/// Wait for `block`'s thread to be gone, then free its stack and `block`. A
/// join the kernel refuses frees nothing, since the stack may still be in use.
///
/// # Safety
/// `block` is a live block whose thread no one else reaps.
unsafe fn reap(block: *mut Block) {
    let b = unsafe { &*block };
    if syscall::thread_join(b.tid.load(Ordering::Relaxed) as u64) == 0 {
        unsafe {
            let _ = syscall::vm_release(b.reservation, b.reserved);
            drop(Box::from_raw(block));
        }
    }
}

impl Thread {
    pub unsafe fn new(stack: usize, init: Box<ThreadInit>) -> io::Result<Thread> {
        let mut ended = REAPED.swap(ptr::null_mut(), Ordering::Acquire);
        while !ended.is_null() {
            // SAFETY: a block on the list was handed over by its ended thread.
            let next = unsafe { (*ended).next };
            unsafe { reap(ended) };
            ended = next;
        }

        // The size asked for, in whole pages, committed before the thread
        // starts so that no touch of it can find no memory.
        let Some(stack_size) = stack.max(1).checked_next_multiple_of(syscall::PAGE_SIZE) else {
            return Err(io::const_error!(io::ErrorKind::InvalidInput, "thread stack size too large"));
        };
        let reserved = stack_size.checked_add(GUARD).ok_or(io::const_error!(
            io::ErrorKind::InvalidInput,
            "thread stack size too large"
        ))?;
        // SAFETY: placed anywhere, so it replaces nothing, and the stack is
        // inside the reservation.
        let reservation = unsafe { syscall::vm_reserve(reserved, syscall::VmProt::None, syscall::VmAt::ANYWHERE) }
            .map_err(|_| io::const_error!(io::ErrorKind::OutOfMemory, "no room for a thread stack"))?;
        let stack_base = reservation.wrapping_add(GUARD);
        if unsafe { syscall::vm_protect(stack_base, stack_size, syscall::VmProt::ReadWrite) }.is_err() {
            let _ = unsafe { syscall::vm_release(reservation, reserved) };
            return Err(io::const_error!(io::ErrorKind::OutOfMemory, "thread stack commit failed"));
        }
        let stack_top = stack_base.expose_provenance() as u64 + stack_size as u64;
        let block = Box::into_raw(Box::new(Block {
            tid: AtomicU32::new(0),
            state: AtomicU32::new(JOINABLE),
            reservation,
            reserved,
            init: UnsafeCell::new(Some(init)),
            next: ptr::null_mut(),
        }));

        // SAFETY: entry point and stack are valid; the block outlives the thread.
        let tid = unsafe {
            syscall::thread_spawn(
                (thread_trampoline as *const ()).expose_provenance() as u64,
                stack_top,
                block.expose_provenance() as u64,
                stack_base.expose_provenance() as u64,
            )
        };

        if syscall::SyscallError::from_u64(tid).is_some() {
            unsafe {
                drop(Box::from_raw(block));
                let _ = syscall::vm_release(reservation, reserved);
            }
            return Err(io::const_error!(io::ErrorKind::Uncategorized, "thread spawn failed"));
        }
        // Before the handle exists: only a dropped handle detaches the
        // thread, so every reaper sees this through the handle or the list.
        unsafe { (*block).tid.store(tid as u32, Ordering::Relaxed) };
        Ok(Thread { block })
    }

    pub fn join(self) {
        let this = ManuallyDrop::new(self);
        // SAFETY: a handle that joins is the only reaper of its thread.
        unsafe { reap(this.block) };
    }
}

impl Drop for Thread {
    fn drop(&mut self) {
        // SAFETY: the block lives until the thread is reaped, which only this
        // handle or, once it is detached, the thread's own exit arranges.
        let b = unsafe { &*self.block };
        if b.state.compare_exchange(JOINABLE, DETACHED, Ordering::AcqRel, Ordering::Acquire).is_err() {
            // It has ended already, so nothing will hand it to `REAPED`.
            unsafe { reap(self.block) };
        }
    }
}

extern "C" fn thread_trampoline(data: u64) {
    let block = crate::ptr::with_exposed_provenance_mut::<Block>(data as usize);
    // SAFETY: the spawner put the init here and only this thread takes it.
    let init = unsafe { (*(*block).init.get()).take() }.expect("a thread's init is taken once");
    let main = init.init();
    main();
    // Run TLS destructors then clean up the current thread handle.
    // The guard module is no-op on ToyOS, so we drive cleanup explicitly.
    unsafe {
        crate::sys::thread_local::destructors::run();
    }
    crate::rt::thread_cleanup();
    // SAFETY: the block is this thread's until it is handed over below.
    if unsafe { &*block }.state.swap(EXITED, Ordering::AcqRel) == DETACHED {
        let mut head = REAPED.load(Ordering::Relaxed);
        loop {
            unsafe { (*block).next = head };
            match REAPED.compare_exchange_weak(head, block, Ordering::Release, Ordering::Relaxed) {
                Ok(_) => break,
                Err(now) => head = now,
            }
        }
    }
    syscall::thread_exit(0);
}

pub fn available_parallelism() -> io::Result<NonZero<usize>> {
    NonZero::new(syscall::cpu_count() as usize)
        .ok_or(io::const_error!(io::ErrorKind::NotFound, "the kernel counts no CPU"))
}

pub fn current_os_id() -> Option<u64> {
    None
}

pub fn yield_now() {
    // No yield syscall yet — spin hint
    core::hint::spin_loop();
}

pub fn set_name(name: &CStr) {
    syscall::set_thread_name(name.to_bytes());
}

pub fn sleep(dur: Duration) {
    let nanos = dur.as_nanos().min(u64::MAX as u128) as u64;
    syscall::nanosleep(nanos);
}
