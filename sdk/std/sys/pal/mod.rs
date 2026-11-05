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
        Refused::File(e) => files_error(e),
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

/// .init_array constructor: registers the EH frame finder for DWARF unwinding
/// in a library, whose loader runs it; an executable's start registers its own.
extern "C" fn init_eh_frame() {
    eh_frame::init();
}

#[used]
#[unsafe(link_section = ".init_array")]
static INIT_EH_FRAME: extern "C" fn() = init_eh_frame;

extern "C" fn start_rust(argc: usize, argv: *const *const u8) -> ! {
    unsafe extern "C" {
        fn main(argc: i32, argv: *const *const u8) -> i32;
        static __ehdr_start: u8;
    }
    // The C library's loader (`sdk/libc/src/dl.rs`), where the program links
    // it: libtoyos_c is built from this std, so std cannot name it itself.
    unsafe extern "C" {
        #[linkage = "extern_weak"]
        static __toyos_load_needed: Option<unsafe extern "C" fn(header: *const u8)>;
    }
    // The program goes by its `argv[0]`.
    let name = match argc {
        0 => &[][..],
        _ => unsafe { crate::ffi::CStr::from_ptr((*argv).cast()) }.to_bytes(),
    };
    // SAFETY: the executable's own header, and this is its main thread.
    unsafe { toyos::tls::start(&raw const __ehdr_start, name) };
    ARGC.store(argc, Ordering::Relaxed);
    ARGV.store(argv.expose_provenance(), Ordering::Relaxed);

    eh_frame::init();
    crate::sys::env::init();
    // Before anything that names a symbol of a library it links.
    // SAFETY: a weak symbol's value, read once.
    match unsafe { __toyos_load_needed } {
        // SAFETY: the executable's own header, its thread-local storage laid out.
        Some(load) => unsafe { load(&raw const __ehdr_start) },
        None if toyos::relocate::names_a_library() => {
            // As `toyos::relocate` refuses its program: a line and 127.
            let _ = crate::io::Write::write_all(&mut crate::io::stderr(), NO_LOADER);
            toyos_abi::syscall::exit(127)
        }
        None => {}
    }

    let code = unsafe { main(argc as i32, argv) };
    crate::sys::stdio::finish();
    toyos_abi::syscall::exit(code)
}

/// What a program that names libraries and links no loader says as it ends.
const NO_LOADER: &[u8] = b"this program names libraries and links no loader: it cannot start\n";

pub fn abort_internal() -> ! {
    toyos_abi::syscall::exit(128 + 6) // SIGABRT-like — kill entire process
}

/// DWARF EH frame finder for the `unwinding` crate: the `.eh_frame_hdr` of the
/// module that holds a PC, off the process's module list (`toyos::tls`).
mod eh_frame {
    struct ToyOsEhFrameFinder;
    static FINDER: ToyOsEhFrameFinder = ToyOsEhFrameFinder;

    unsafe impl unwind::EhFrameFinder for ToyOsEhFrameFinder {
        fn find(&self, pc: usize) -> Option<unwind::FrameInfo> {
            let module = toyos::tls::modules().find(|m| m.holds(pc))?;
            (module.image.eh_frame_hdr != 0).then(|| unwind::FrameInfo {
                text_base: Some(module.image.base),
                kind: unwind::FrameInfoKind::EhFrameHdr(module.image.eh_frame_hdr),
            })
        }
    }

    pub(super) fn init() {
        unwind::set_custom_eh_frame_finder(&FINDER).ok();
    }
}
