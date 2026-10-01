#![no_std]
#![feature(thread_local)]
#![cfg_attr(not(feature = "std-runtime"), feature(linkage))]

extern crate alloc;

mod arch;
mod ctype;
mod elfsym;
mod errno;
mod fdreq;
mod fparts;
mod link;
mod listing;
mod locale;
mod math;
mod memory;
mod memreq;
mod misc;
mod posix_io;
mod printf;
mod pthread;
mod refused;
mod sigmask;
mod socket;
mod stdio;
mod string;
mod strtonum;
mod text;
mod time;
mod utf8;
mod wchar;

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

    struct MmapAllocator;

    unsafe impl dlmalloc::Allocator for MmapAllocator {
        fn alloc(&self, size: usize) -> (*mut u8, usize, u32) {
            use toyos_abi::syscall::{MmapProt, MmapFlags};
            let ptr = unsafe {
                toyos_abi::syscall::mmap(
                    core::ptr::null_mut(),
                    size,
                    MmapProt::READ | MmapProt::WRITE,
                    MmapFlags::ANONYMOUS,
                )
            };
            if ptr.is_null() { (core::ptr::null_mut(), 0, 0) } else { (ptr, size, 0) }
        }

        fn remap(&self, _ptr: *mut u8, _old: usize, _new: usize, _can_move: bool) -> *mut u8 {
            core::ptr::null_mut()
        }

        fn free_part(&self, _ptr: *mut u8, _old: usize, _new: usize) -> bool {
            false
        }

        fn free(&self, ptr: *mut u8, size: usize) -> bool {
            unsafe { toyos_abi::syscall::munmap(ptr, size).is_ok() }
        }

        fn can_release_part(&self, _flags: u32) -> bool {
            false
        }

        fn allocates_zeros(&self) -> bool {
            true
        }

        fn page_size(&self) -> usize {
            0x1000
        }
    }

    struct SyncDlmalloc(core::cell::UnsafeCell<dlmalloc::Dlmalloc<MmapAllocator>>);
    unsafe impl Sync for SyncDlmalloc {}

    static DLMALLOC: SyncDlmalloc =
        SyncDlmalloc(core::cell::UnsafeCell::new(dlmalloc::Dlmalloc::new_with_allocator(MmapAllocator)));
    static LOCKED: core::sync::atomic::AtomicI32 = core::sync::atomic::AtomicI32::new(0);

    fn lock() -> DropLock {
        while LOCKED.swap(1, core::sync::atomic::Ordering::Acquire) != 0 {
            core::hint::spin_loop();
        }
        DropLock
    }

    struct DropLock;
    impl Drop for DropLock {
        fn drop(&mut self) {
            LOCKED.store(0, core::sync::atomic::Ordering::Release);
        }
    }

    struct LibcAllocator;

    #[global_allocator]
    static ALLOCATOR: LibcAllocator = LibcAllocator;

    unsafe impl core::alloc::GlobalAlloc for LibcAllocator {
        unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
            let _lock = lock();
            unsafe { (*DLMALLOC.0.get()).malloc(layout.size(), layout.align()) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: core::alloc::Layout) {
            let _lock = lock();
            unsafe { (*DLMALLOC.0.get()).free(ptr, layout.size(), layout.align()) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: core::alloc::Layout, new_size: usize) -> *mut u8 {
            let _lock = lock();
            unsafe { (*DLMALLOC.0.get()).realloc(ptr, layout.size(), layout.align(), new_size) }
        }
    }
}
