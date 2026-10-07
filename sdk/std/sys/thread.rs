use toyos_abi::syscall;

use crate::ffi::CStr;
use crate::io;
use crate::num::NonZero;
use crate::thread::ThreadInit;
use crate::time::Duration;

pub struct Thread {
    tid: u32,
}

unsafe impl Send for Thread {}
unsafe impl Sync for Thread {}

pub const DEFAULT_MIN_STACK_SIZE: usize = 2 * 1024 * 1024;

impl Thread {
    pub unsafe fn new(stack: usize, init: Box<ThreadInit>) -> io::Result<Thread> {
        let stack_size = stack.max(DEFAULT_MIN_STACK_SIZE);
        // Allocate user stack (page-aligned)
        let layout = crate::alloc::Layout::from_size_align(stack_size, 4096).unwrap();
        let stack_base = unsafe { crate::alloc::alloc(layout) };
        if stack_base.is_null() {
            return Err(io::const_error!(
                io::ErrorKind::OutOfMemory,
                "thread stack allocation failed"
            ));
        }
        let stack_top = stack_base.expose_provenance() as u64 + stack_size as u64;

        let data = Box::into_raw(init);
        // SAFETY: entry point and stack are valid; data is a valid pointer to the thread init.
        let tid = unsafe {
            syscall::thread_spawn(
                (thread_trampoline as *const ()).expose_provenance() as u64,
                stack_top,
                data.expose_provenance() as u64,
                stack_base.expose_provenance() as u64,
            )
        };

        if syscall::SyscallError::from_u64(tid).is_some() {
            unsafe {
                drop(Box::from_raw(data));
            }
            unsafe {
                crate::alloc::dealloc(stack_base, layout);
            }
            return Err(io::const_error!(io::ErrorKind::Uncategorized, "thread spawn failed"));
        }

        Ok(Thread { tid: tid as u32 })
    }

    pub fn join(self) {
        syscall::thread_join(self.tid as u64);
    }
}

extern "C" fn thread_trampoline(data: u64) {
    let init = unsafe {
        Box::from_raw(crate::ptr::with_exposed_provenance_mut::<ThreadInit>(data as usize))
    };
    let main = init.init();
    main();
    // Run TLS destructors then clean up the current thread handle.
    // The guard module is no-op on ToyOS, so we drive cleanup explicitly.
    unsafe {
        crate::sys::thread_local::destructors::run();
    }
    crate::rt::thread_cleanup();
    syscall::thread_exit(0);
}

pub fn available_parallelism() -> io::Result<NonZero<usize>> {
    // ToyOS runs on QEMU with a known number of CPUs, but we don't expose
    // a syscall for this yet. Return 1 for now.
    Ok(unsafe { NonZero::new_unchecked(1) })
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
