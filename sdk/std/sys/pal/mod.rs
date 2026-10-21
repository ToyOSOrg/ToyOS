mod c_allocator;
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
        // A read of a connection met handles the peer sent with no frame announcing them.
        SyscallError::HandlesPending => ErrorKind::InvalidData,
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
