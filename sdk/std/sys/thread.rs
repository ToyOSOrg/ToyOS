use toyos_abi::syscall;

use crate::cell::UnsafeCell;
use crate::ffi::CStr;
use crate::io;
use crate::mem::ManuallyDrop;
use crate::num::NonZero;
use crate::sync::atomic::{AtomicU32, Ordering};
use crate::thread::ThreadInit;
use crate::time::Duration;

/// A thread's handle. Joined, it frees the thread's stack; dropped, it
/// detaches the thread, which frees its own as it ends.
pub struct Thread {
    block: *mut Block,
}

unsafe impl Send for Thread {}
unsafe impl Sync for Thread {}

/// What a thread and its handle share. Freed by its joiner, or by a detached
/// thread itself as it ends.
struct Block {
    tid: AtomicU32,
    state: AtomicU32,
    /// The thread's memory (`toyos::tls::ThreadMemory`): its area, a guard and its stack.
    reservation: *mut u8,
    reserved: usize,
    /// What the thread names as its thread pointer first.
    tp: usize,
    /// Taken by the thread as it starts.
    init: UnsafeCell<Option<Box<ThreadInit>>>,
}

const JOINABLE: u32 = 0;
const DETACHED: u32 = 1;
const EXITED: u32 = 2;

pub const DEFAULT_MIN_STACK_SIZE: usize = 2 * 1024 * 1024;

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
        // The stack asked for, in whole pages, and the thread's area, committed
        // before the thread starts so that no touch of either can find no
        // memory. One guard page is enough below the stack, since every frame
        // larger than a page probes each page it spans (the target's inline
        // stack probes).
        let Some(memory) = toyos::tls::ThreadMemory::reserve(stack) else {
            return Err(io::const_error!(io::ErrorKind::OutOfMemory, "no memory for a thread"));
        };
        let toyos::tls::ThreadMemory { reservation, reserved, stack_base, stack_top, tp } = memory;
        let block = Box::into_raw(Box::new(Block {
            tid: AtomicU32::new(0),
            state: AtomicU32::new(JOINABLE),
            reservation,
            reserved,
            tp,
            init: UnsafeCell::new(Some(init)),
        }));

        // SAFETY: entry point and stack are valid; the block outlives the thread.
        let tid = unsafe {
            syscall::thread_spawn(
                (thread_trampoline as *const ()).expose_provenance() as u64,
                stack_top.expose_provenance() as u64,
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
        // thread, and a detached thread reads no tid.
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
        // handle or, once it is detached, the thread's own exit does.
        let b = unsafe { &*self.block };
        if b.state.compare_exchange(JOINABLE, DETACHED, Ordering::AcqRel, Ordering::Acquire).is_err() {
            // It has ended already, so it left itself to be joined.
            unsafe { reap(self.block) };
        }
    }
}

extern "C" fn thread_trampoline(data: u64) {
    let block = crate::ptr::with_exposed_provenance_mut::<Block>(data as usize);
    // Before anything reads a thread-local.
    // SAFETY: the spawner wrote the block, which outlives this thread.
    if toyos::tls::enter(unsafe { (*block).tp }).is_err() {
        rtabort!("the kernel refused a thread's thread pointer");
    }
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
    toyos::tls::leave();
    // SAFETY: the block is this thread's until its state says otherwise.
    if unsafe { &*block }.state.swap(EXITED, Ordering::AcqRel) == DETACHED {
        // Nobody holds the block but this thread, which frees it and leaves
        // its memory to the kernel to release.
        let Block { reservation, reserved, .. } = *unsafe { Box::from_raw(block) };
        // SAFETY: the stack is this thread's alone, and ends with it.
        let refused = unsafe { syscall::thread_exit_detached(0, reservation, reserved) };
        rtabort!("a detached thread's stack was refused at its exit: {refused:?}");
    }
    syscall::thread_exit(0);
}

/// The CPUs the calling thread may run on, as Linux's counts its affinity.
pub fn available_parallelism() -> io::Result<NonZero<usize>> {
    let mask = syscall::thread_affinity(u64::from(toyos::tls::current_tid().0))
        .map_err(|_| io::const_error!(io::ErrorKind::NotFound, "the kernel reads no mask for this thread"))?;
    NonZero::new(mask.iter().map(|word| word.count_ones() as usize).sum())
        .ok_or(io::const_error!(io::ErrorKind::NotFound, "the thread's mask names no CPU"))
}

pub fn current_os_id() -> Option<u64> {
    None
}

pub fn yield_now() {
    syscall::yield_now();
}

pub fn set_name(name: &CStr) {
    syscall::set_thread_name(name.to_bytes());
}

pub fn sleep(dur: Duration) {
    let nanos = dur.as_nanos().min(u64::MAX as u128) as u64;
    syscall::nanosleep(nanos);
}
