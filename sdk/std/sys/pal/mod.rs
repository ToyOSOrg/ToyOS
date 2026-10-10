pub mod futex;
pub mod os;
pub mod tls;

#[expect(dead_code)]
#[path = "../../../../rust/library/std/src/sys/pal/unsupported/common.rs"]
mod unsupported_common;

use core::sync::atomic::{AtomicUsize, Ordering};

pub use unsupported_common::{cleanup, init};

/// The kernel's word as the `ErrorKind` a caller can act on.
///
/// Exhaustive: a new `SyscallError` has to be given a kind here rather than
/// reaching callers as `Other` from whichever module mapped it last.
pub fn to_io_error(e: toyos_abi::syscall::SyscallError) -> crate::io::Error {
    use toyos_abi::syscall::SyscallError;

    use crate::io::ErrorKind;

    let kind = match e {
        SyscallError::Unknown => ErrorKind::Uncategorized,
        SyscallError::NotFound => ErrorKind::NotFound,
        SyscallError::PermissionDenied => ErrorKind::PermissionDenied,
        SyscallError::AlreadyExists => ErrorKind::AlreadyExists,
        SyscallError::InvalidArgument => ErrorKind::InvalidInput,
        SyscallError::BadAddress => ErrorKind::InvalidInput,
        SyscallError::WouldBlock => ErrorKind::WouldBlock,
        SyscallError::ResourceExhausted => ErrorKind::OutOfMemory,
        SyscallError::NotSupported => ErrorKind::Unsupported,
        SyscallError::Io => ErrorKind::Other,
        SyscallError::Gone => ErrorKind::BrokenPipe,
    };
    crate::io::Error::from(kind)
}

// argc/argv stored by _start for std::env::args()
pub(crate) static ARGC: AtomicUsize = AtomicUsize::new(0);
pub(crate) static ARGV: AtomicUsize = AtomicUsize::new(0); // *const *const u8 as usize

// Stack layout at entry (set up by kernel), with the stack pointer 16-byte aligned:
//   [sp]   = argc
//   [sp+8] = argv[0], argv[1], ..., NULL
// The image relocates itself first, from its own header's address.
#[cfg(target_arch = "x86_64")]
#[unsafe(no_mangle)]
#[unsafe(naked)]
unsafe extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "lea rdi, [rip + __ehdr_start]",
        "call {relocate}",
        "mov rdi, [rsp]",
        "lea rsi, [rsp + 8]",
        "call {start_rust}",
        "ud2",
        relocate = sym toyos::relocate::relocate_self,
        start_rust = sym start_rust,
    );
}

#[cfg(target_arch = "aarch64")]
#[unsafe(no_mangle)]
#[unsafe(naked)]
unsafe extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "adrp x0, __ehdr_start",
        "add x0, x0, :lo12:__ehdr_start",
        "bl {relocate}",
        "ldr x0, [sp]",
        "add x1, sp, #8",
        // The outermost frame record: a backtrace ends here.
        "mov x29, xzr",
        "mov x30, xzr",
        "bl {start_rust}",
        "brk #0x1",
        relocate = sym toyos::relocate::relocate_self,
        start_rust = sym start_rust,
    );
}

/// .init_array constructor: registers the EH frame finder for DWARF unwinding.
/// For executables, this runs before `_start`. For cdylib .so files loaded via
/// dlopen, the kernel returns the .init_array to userspace which calls it.
/// This ensures panic unwinding works from code inside shared libraries.
extern "C" fn init_eh_frame() {
    eh_frame::init();
}

#[used]
#[unsafe(link_section = ".init_array")]
static INIT_EH_FRAME: extern "C" fn() = init_eh_frame;

extern "C" fn start_rust(argc: usize, argv: *const *const u8) -> ! {
    unsafe extern "C" {
        fn main(argc: i32, argv: *const *const u8) -> i32;
    }
    ARGC.store(argc, Ordering::Relaxed);
    ARGV.store(argv.expose_provenance(), Ordering::Relaxed);

    // Register EH frame finder (also in .init_array for cdylib, but exes don't run .init_array)
    eh_frame::init();

    // Initialize environment variables
    crate::sys::env::init();

    let code = unsafe { main(argc as i32, argv) };
    crate::sys::stdio::finish();
    toyos_abi::syscall::exit(code)
}

pub fn abort_internal() -> ! {
    toyos_abi::syscall::exit(128 + 6) // SIGABRT-like — kill entire process
}

// C allocator shims — many crates (zlib-rs, etc.) call malloc/free/calloc
// via extern "C". Route through the Rust global allocator (arena+slab)
// to avoid per-allocation syscalls.
mod c_allocator {
    use crate::alloc::{GlobalAlloc, Layout, System};

    /// The alignment every block has at least, and the bytes in front of a
    /// block that hold its size and its alignment: the layout `free` and
    /// `realloc` release it at is the one it was allocated with.
    const MIN_ALIGN: usize = 16;

    /// A block of `size` bytes at `align`, the allocation beginning `align`
    /// bytes before it.
    fn layout(size: usize, align: usize) -> Option<Layout> {
        Layout::from_size_align(align.checked_add(size)?, align).ok()
    }

    /// The size and alignment in front of `block`.
    unsafe fn header(block: *mut u8) -> (usize, usize) {
        unsafe { ((block.sub(16) as *const usize).read(), (block.sub(8) as *const usize).read()) }
    }

    /// `size` bytes aligned to `align`, a power of two.
    unsafe fn alloc(size: usize, align: usize) -> *mut u8 {
        let align = align.max(MIN_ALIGN);
        let Some(layout) = layout(size, align) else { return core::ptr::null_mut() };
        let raw = unsafe { System.alloc(layout) };
        if raw.is_null() {
            return raw;
        }
        unsafe {
            let block = raw.add(align);
            (block.sub(16) as *mut usize).write(size);
            (block.sub(8) as *mut usize).write(align);
            block
        }
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn malloc(size: usize) -> *mut u8 {
        if size == 0 {
            return core::ptr::null_mut();
        }
        unsafe { alloc(size, MIN_ALIGN) }
    }

    /// C11's: null for an alignment that is no power of two.
    #[unsafe(no_mangle)]
    unsafe extern "C" fn aligned_alloc(align: usize, size: usize) -> *mut u8 {
        if !align.is_power_of_two() {
            return core::ptr::null_mut();
        }
        unsafe { alloc(size, align) }
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn calloc(count: usize, size: usize) -> *mut u8 {
        let total = count.saturating_mul(size);
        let ptr = malloc(total);
        if !ptr.is_null() && total > 0 {
            unsafe { core::ptr::write_bytes(ptr, 0, total) };
        }
        ptr
    }

    #[unsafe(no_mangle)]
    unsafe extern "C" fn free(ptr: *mut u8) {
        if ptr.is_null() {
            return;
        }
        let (size, align) = unsafe { header(ptr) };
        let layout = layout(size, align).expect("a block's header is the layout it was allocated with");
        unsafe { System.dealloc(ptr.sub(align), layout) };
    }

    /// `System.realloc` keeps the allocation's alignment, so the block stays
    /// `align` bytes in, its header carried with its bytes.
    #[unsafe(no_mangle)]
    unsafe extern "C" fn realloc(ptr: *mut u8, new_size: usize) -> *mut u8 {
        if ptr.is_null() {
            return malloc(new_size);
        }
        if new_size == 0 {
            free(ptr);
            return core::ptr::null_mut();
        }
        let (size, align) = unsafe { header(ptr) };
        let Some(new) = layout(new_size, align) else { return core::ptr::null_mut() };
        let old = layout(size, align).expect("a block's header is the layout it was allocated with");
        let raw = unsafe { System.realloc(ptr.sub(align), old, new.size()) };
        if raw.is_null() {
            return raw;
        }
        unsafe {
            let block = raw.add(align);
            (block.sub(16) as *mut usize).write(new_size);
            block
        }
    }
}

/// DWARF EH frame finder for the `unwinding` crate.
/// Locates `.eh_frame_hdr` for a given PC via `SYS_QUERY_MODULES`.
mod eh_frame {
    use toyos_abi::syscall::ModuleInfo;

    use crate::sync::Mutex;

    struct Module {
        base: usize,
        end: usize,
        eh_frame_hdr: usize,
        eh_frame_hdr_size: usize,
    }

    static CACHE: Mutex<Vec<Module>> = Mutex::new(Vec::new());

    fn load_modules() -> Vec<Module> {
        let mut buf = vec![0u8; 4096];
        loop {
            match toyos_abi::syscall::query_modules(&mut buf) {
                Ok(count) => {
                    let info_size = core::mem::size_of::<ModuleInfo>();
                    let mut modules = Vec::with_capacity(count);
                    for i in 0..count {
                        let off = i * info_size;
                        if off + info_size > buf.len() {
                            break;
                        }
                        let info = unsafe { &*(buf.as_ptr().add(off) as *const ModuleInfo) };
                        modules.push(Module {
                            base: info.base as usize,
                            end: info.text_end as usize,
                            eh_frame_hdr: info.eh_frame_hdr as usize,
                            eh_frame_hdr_size: info.eh_frame_hdr_size as usize,
                        });
                    }
                    return modules;
                }
                Err(_) => {
                    buf.resize(buf.len() * 2, 0);
                    if buf.len() > 1024 * 1024 {
                        return Vec::new();
                    }
                }
            }
        }
    }

    struct ToyOsEhFrameFinder;
    static FINDER: ToyOsEhFrameFinder = ToyOsEhFrameFinder;

    unsafe impl unwind::EhFrameFinder for ToyOsEhFrameFinder {
        fn find(&self, pc: usize) -> Option<unwind::FrameInfo> {
            // Fast path: check cached modules
            {
                let cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(m) = cache.iter().find(|m| pc >= m.base && pc < m.end) {
                    if m.eh_frame_hdr != 0 {
                        return Some(unwind::FrameInfo {
                            text_base: Some(m.base),
                            kind: unwind::FrameInfoKind::EhFrameHdr(m.eh_frame_hdr),
                        });
                    }
                    return None;
                }
            }

            // Cache miss — reload module list (handles dlopen)
            let modules = load_modules();
            let result = modules.iter().find(|m| pc >= m.base && pc < m.end).and_then(|m| {
                if m.eh_frame_hdr != 0 {
                    Some(unwind::FrameInfo {
                        text_base: Some(m.base),
                        kind: unwind::FrameInfoKind::EhFrameHdr(m.eh_frame_hdr),
                    })
                } else {
                    None
                }
            });
            *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = modules;
            result
        }
    }

    pub(super) fn init() {
        let modules = load_modules();
        *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = modules;
        unwind::set_custom_eh_frame_finder(&FINDER).ok();
    }
}
