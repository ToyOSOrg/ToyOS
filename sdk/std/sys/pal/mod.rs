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
    };
    crate::io::Error::from(kind)
}

/// `f` on the kernel handle descriptor `fd` names, held for the call
/// (`toyos::fd::with`): a descriptor that is not open answers EBADF's error,
/// and one a library keeps as no kernel handle is no file to a kernel call.
pub fn on_fd<T>(
    fd: i32,
    f: impl FnOnce(toyos::BorrowedHandle<'_>) -> Result<T, toyos_abi::syscall::SyscallError>,
) -> crate::io::Result<T> {
    let answer = toyos::fd::with(fd, |object| match object.kernel_handle() {
        Some(handle) => f(handle).map_err(to_io_error),
        None => Err(crate::io::const_error!(
            crate::io::ErrorKind::Unsupported,
            "the descriptor names an object of a library's own",
        )),
    });
    answer.map_err(fd_error)?
}

/// A file server's refusal, or the client end's own, as std says it.
pub fn served_error(e: toyos::fs::Error) -> crate::io::Error {
    use crate::io::{self, ErrorKind as Kind};
    use toyos::fs::Error;
    match e {
        Error::NotFound => io::const_error!(Kind::NotFound, "no such file or directory"),
        Error::AlreadyExists => io::const_error!(Kind::AlreadyExists, "it already exists"),
        Error::NotADirectory => io::const_error!(Kind::NotADirectory, "not a directory"),
        Error::IsADirectory => io::const_error!(Kind::IsADirectory, "is a directory"),
        Error::NotEmpty => io::const_error!(Kind::DirectoryNotEmpty, "the directory is not empty"),
        Error::NotALink => io::const_error!(Kind::InvalidInput, "not a symbolic link"),
        Error::NameTooLong => io::const_error!(Kind::InvalidFilename, "the name or path is too long"),
        Error::InvalidName => io::const_error!(Kind::InvalidFilename, "the volume cannot store that name"),
        Error::ReadOnly => io::const_error!(Kind::PermissionDenied, "the directory is read-only to this process"),
        Error::NotOpenForWriting => io::const_error!(Kind::PermissionDenied, "the file is not open for writing"),
        Error::Root => io::const_error!(Kind::ResourceBusy, "a directory the file server serves stays where it is"),
        Error::Escapes => io::const_error!(Kind::PermissionDenied, "a link leads out of the directory"),
        Error::CrossesDirectories => {
            io::const_error!(Kind::CrossesDevices, "a link leads to another directory capability")
        }
        Error::Loop => io::const_error!(Kind::FilesystemLoop, "too many symbolic links"),
        Error::IntoItself => io::const_error!(Kind::InvalidInput, "a directory cannot move beneath itself"),
        Error::NoSpace => io::const_error!(Kind::StorageFull, "the volume is full"),
        Error::FileTooLarge => io::const_error!(Kind::FileTooLarge, "the file would be too large"),
        Error::LimitExceeded => io::const_error!(Kind::QuotaExceeded, "past a bound the file server keeps"),
        Error::TooManyOpen => io::const_error!(Kind::QuotaExceeded, "too many files open on the file server"),
        Error::TooManyConnections => {
            io::const_error!(Kind::QuotaExceeded, "the file server serves as many connections as it takes")
        }
        Error::TooManyStreams => {
            io::const_error!(Kind::QuotaExceeded, "the file server serves as many streams as it takes")
        }
        Error::Io => io::const_error!(Kind::Other, "the volume's device failed, or its structures do not decode"),
        Error::Stale => io::const_error!(
            Kind::StaleNetworkFileHandle,
            "the file was removed or replaced, or its file server restarted, since it was opened",
        ),
        Error::UnknownFile => io::const_error!(Kind::InvalidInput, "the file server holds no such open file"),
        Error::InvalidPath => io::const_error!(Kind::InvalidInput, "the path is not one the file server takes"),
        Error::InvalidRequest => io::const_error!(Kind::InvalidInput, "the file server takes no such request"),
        Error::Unsupported => io::const_error!(Kind::Unsupported, "the volume's format has no such thing"),
        Error::OutOfResources => io::const_error!(Kind::OutOfMemory, "the kernel had no room for the request"),
        Error::NoDirectory => {
            io::const_error!(Kind::NotADirectory, "the connection was given open files and no directory")
        }
        Error::Gone => io::const_error!(Kind::StaleNetworkFileHandle, "the file server is gone"),
        Error::NotHeld => io::const_error!(Kind::NotFound, "this process holds no such directory"),
        Error::Malformed => io::const_error!(Kind::InvalidData, "the file server answered outside its protocol"),
    }
}

/// `toyos::files`' refusal, as std says it.
pub fn files_error(e: toyos::files::Error) -> crate::io::Error {
    use crate::io;
    use toyos::files;
    match e {
        files::Error::Kernel(e) => to_io_error(e),
        files::Error::Served(e) => served_error(e),
        files::Error::Fd(e) => fd_error(e),
        files::Error::NotUtf8 => io::const_error!(io::ErrorKind::InvalidInput, "path is not UTF-8"),
        files::Error::Loop => io::const_error!(io::ErrorKind::FilesystemLoop, "too many symbolic links"),
        files::Error::CrossesServers => {
            io::const_error!(io::ErrorKind::CrossesDevices, "the two paths are on different servers")
        }
        files::Error::NotAFile => io::const_error!(io::ErrorKind::InvalidInput, "the descriptor names no file"),
        files::Error::NotADirectory => {
            io::const_error!(io::ErrorKind::NotADirectory, "the descriptor names no directory")
        }
        files::Error::Stream(refused) => crate::sys::net::stream_refused(refused),
    }
}

/// Why nothing was spawned, as std's error.
pub fn refused(refused: toyos::spawn::Refused) -> crate::io::Error {
    use toyos::spawn::Refused;

    use crate::io::{Error, ErrorKind};
    match refused {
        Refused::Permission(why) => Error::new(ErrorKind::PermissionDenied, why),
        Refused::Invalid(why) => Error::new(ErrorKind::InvalidInput, why),
        Refused::Stream(handle) => {
            Error::new(ErrorKind::InvalidInput, toyos::net::stream::Misuse::Moved(handle).to_string())
        }
        Refused::Export(e) => match e {
            toyos::net::NetError::NetstackNotFound => Error::from(ErrorKind::ConnectionAborted),
            toyos::net::NetError::ResourceExhausted => Error::from(ErrorKind::QuotaExceeded),
            _ => Error::from(ErrorKind::Other),
        },
        Refused::NotStarted => Error::from(ErrorKind::Other),
        Refused::Syscall(e) => to_io_error(e),
    }
}

/// A kernel handle this process just got, as a descriptor of its own: std
/// creates descriptors not inherited, as std is close-on-exec on unix.
pub fn new_fd(handle: toyos::OwnedHandle) -> crate::io::Result<i32> {
    toyos::fd::install(toyos::fd::Object::handle(handle), false, 0).map_err(fd_error)
}

/// The descriptor table's refusal as std's error.
pub fn fd_error(e: toyos::fd::Error) -> crate::io::Error {
    match e {
        toyos::fd::Error::BadFd => {
            crate::io::const_error!(crate::io::ErrorKind::Uncategorized, "the descriptor is not open")
        }
        toyos::fd::Error::Full => {
            crate::io::const_error!(crate::io::ErrorKind::QuotaExceeded, "the descriptor table is full")
        }
    }
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
