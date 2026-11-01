#![no_std]
#![feature(thread_local)]
#![cfg_attr(not(feature = "std-runtime"), feature(linkage))]

extern crate alloc;

mod affinity;
mod alarmreq;
mod signal;
mod sigreq;
mod errtext;
mod arch;
mod atreq;
mod conversion;
mod ctype;
mod dgramreq;
mod elfsym;
mod env;
mod errcode;
mod errno;
mod fdreq;
mod fparts;
mod inaddr;
mod link;
mod linkreq;
mod listing;
mod locale;
mod math;
mod memory;
mod memreq;
mod misc;
mod pollreq;
mod posix_io;
mod printf;
mod pthread;
mod refused;
mod resolve;
mod sigmask;
mod socket;
mod sockopt;
mod spawn;
mod spawnreq;
mod stdio;
mod string;
mod strtonum;
mod text;
mod time;
mod uname;
mod utf8;
mod wchar;

#[cfg(not(feature = "std-runtime"))]
mod mapped;

// C runtime: the entry `arch::_start` calls, panic handler, and global allocator.
// Only for pure C programs (no Rust std). When linked into a Rust program
// with std, std provides these.
#[cfg(not(feature = "std-runtime"))]
mod runtime {
    use core::panic::PanicInfo;

    // Returning from `main` is defined as calling `exit` with its value, so this
    // goes through libc's `exit` rather than the syscall: the atexit table and
    // `fflush(NULL)` are what stand between a program's last unterminated line
    // and the fd.
    pub(crate) extern "C" fn start_c(argc: i32, argv: *const *const u8) -> ! {
        unsafe extern "C" {
            fn main(argc: i32, argv: *const *const u8) -> i32;
        }
        // Before the constructors and `main`, either of which may read
        // `environ` before any call that would make it.
        crate::env::init();
        // SAFETY: the linker's bounds of the program's constructor array.
        unsafe {
            for hook in hooks(&raw const __init_array_start, &raw const __init_array_end) {
                hook();
            }
        }
        let code = unsafe { main(argc, argv) };
        unsafe { crate::misc::exit(code) }
    }

    /// A constructor or destructor, as `.init_array` and `.fini_array` hold them.
    type Hook = unsafe extern "C" fn();

    // lld defines each pair for any link that names it, equal when the section
    // is absent.
    unsafe extern "C" {
        static __init_array_start: Hook;
        static __init_array_end: Hook;
        static __fini_array_start: Hook;
        static __fini_array_end: Hook;
    }

    /// # Safety
    /// `start` and `end` bound one array of hooks.
    unsafe fn hooks(start: *const Hook, end: *const Hook) -> &'static [Hook] {
        let len = (end.addr() - start.addr()) / core::mem::size_of::<Hook>();
        // SAFETY: the caller's contract.
        unsafe { core::slice::from_raw_parts(start, len) }
    }

    /// Run `.fini_array`, last entry first: `exit` calls it after the `atexit`
    /// handlers and before the streams are flushed.
    pub(crate) unsafe fn fini() {
        // SAFETY: the linker's bounds of the program's destructor array.
        for hook in unsafe { hooks(&raw const __fini_array_start, &raw const __fini_array_end) }.iter().rev() {
            unsafe { hook() };
        }
    }

    struct Stderr;

    impl core::fmt::Write for Stderr {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            let _ = crate::posix_io::write_fd(2, s.as_bytes());
            Ok(())
        }
    }

    #[panic_handler]
    fn panic(info: &PanicInfo) -> ! {
        use core::fmt::Write;
        let _ = write!(Stderr, "libc panic: {info}\n");
        toyos_abi::syscall::exit(134) // SIGABRT-like
    }

    /// The personality `core`'s unwind tables name. This library unwinds
    /// nothing of its own, and an exception that reaches one of its Rust
    /// frames ends the program here, loudly.
    #[unsafe(no_mangle)]
    extern "C" fn rust_eh_personality() -> ! {
        use core::fmt::Write;
        let _ = write!(Stderr, "libc: an exception unwound into Rust code, which cannot catch it\n");
        toyos_abi::syscall::exit(134)
    }

    /// What the precompiled `alloc`'s landing pads name, so a program with no
    /// unwinder links. Weak: libunwind's `UnwindLevel1.o` defines it strongly
    /// beside `_Unwind_RaiseException`, the only way an unwind starts, so this
    /// one stays the program's only while nothing can unwind.
    #[unsafe(no_mangle)]
    #[linkage = "weak"]
    extern "C" fn _Unwind_Resume() -> ! {
        use core::fmt::Write;
        let _ = write!(Stderr, "libc: _Unwind_Resume with no unwinder linked\n");
        toyos_abi::syscall::exit(134)
    }

    /// dlmalloc's memory: the process's own private pages, committed whole and
    /// populated as they are touched, and given back in part when it trims.
    struct VmAllocator;

    unsafe impl dlmalloc::Allocator for VmAllocator {
        fn alloc(&self, size: usize) -> (*mut u8, usize, u32) {
            use toyos_abi::syscall::{self, VmAt, VmProt};
            // SAFETY: placed anywhere, so it replaces nothing.
            match unsafe { syscall::vm_reserve(size, VmProt::ReadWrite, VmAt::ANYWHERE) } {
                Ok(ptr) => (ptr, size, 0),
                Err(_) => (core::ptr::null_mut(), 0, 0),
            }
        }

        fn remap(&self, _ptr: *mut u8, _old: usize, _new: usize, _can_move: bool) -> *mut u8 {
            core::ptr::null_mut()
        }

        fn free_part(&self, ptr: *mut u8, old: usize, new: usize) -> bool {
            // SAFETY: dlmalloc gives up the tail of a segment it no longer uses.
            unsafe { toyos_abi::syscall::vm_release(ptr.wrapping_add(new), old - new).is_ok() }
        }

        fn free(&self, ptr: *mut u8, size: usize) -> bool {
            // SAFETY: dlmalloc gives up a whole segment it no longer uses.
            unsafe { toyos_abi::syscall::vm_release(ptr, size).is_ok() }
        }

        fn can_release_part(&self, _flags: u32) -> bool {
            true
        }

        fn allocates_zeros(&self) -> bool {
            true
        }

        fn page_size(&self) -> usize {
            toyos_abi::syscall::PAGE_SIZE
        }
    }

    /// A contended allocation sleeps on the lock's futex rather than spinning.
    static DLMALLOC: toyos::sync::Lock<dlmalloc::Dlmalloc<VmAllocator>> =
        toyos::sync::Lock::new(dlmalloc::Dlmalloc::new_with_allocator(VmAllocator));

    pub(crate) struct LibcAllocator;

    #[global_allocator]
    pub(crate) static ALLOCATOR: LibcAllocator = LibcAllocator;

    use super::mapped;

    unsafe impl core::alloc::GlobalAlloc for LibcAllocator {
        unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
            if mapped::holds(layout) {
                return mapped::alloc(layout);
            }
            unsafe { DLMALLOC.lock().malloc(layout.size(), layout.align()) }
        }
        unsafe fn alloc_zeroed(&self, layout: core::alloc::Layout) -> *mut u8 {
            if mapped::holds(layout) {
                return mapped::alloc(layout);
            }
            unsafe { DLMALLOC.lock().calloc(layout.size(), layout.align()) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: core::alloc::Layout) {
            if mapped::holds(layout) {
                return unsafe { mapped::dealloc(ptr, layout) };
            }
            unsafe { DLMALLOC.lock().free(ptr, layout.size(), layout.align()) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: core::alloc::Layout, new_size: usize) -> *mut u8 {
            let small = || unsafe { DLMALLOC.lock().realloc(ptr, layout.size(), layout.align(), new_size) };
            unsafe { mapped::realloc(self, ptr, layout, new_size, small) }
        }
    }
}
