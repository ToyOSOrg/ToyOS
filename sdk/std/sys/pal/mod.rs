mod c_allocator;
pub mod futex;
pub mod os;
pub mod tls;

#[expect(dead_code)]
#[path = "../../../../rust/library/std/src/sys/pal/unsupported/common.rs"]
mod unsupported_common;

pub use unsupported_common::{cleanup, init};

/// A refusal as std says it: the kernel's, a file server's, netstack's or the
/// SDK's own, each read as its `toyos::Failure` first, so every module answers
/// one refusal with one kind.
///
/// Exhaustive: a new `Failure` has to be given a kind here.
pub fn failed(e: impl Into<toyos::Failure>) -> crate::io::Error {
    use toyos::Failure;

    use crate::io::ErrorKind as Kind;
    use crate::io::const_error;

    match e.into() {
        Failure::NotFound => const_error!(Kind::NotFound, "no such file, name or object"),
        Failure::PermissionDenied => const_error!(Kind::PermissionDenied, "not permitted"),
        Failure::AlreadyExists => const_error!(Kind::AlreadyExists, "it already exists"),
        Failure::InvalidArgument => const_error!(Kind::InvalidInput, "an argument the call does not take"),
        Failure::BadAddress => const_error!(Kind::InvalidInput, "an address the kernel could not reach"),
        Failure::WouldBlock => Kind::WouldBlock.into(),
        Failure::OutOfMemory => const_error!(Kind::OutOfMemory, "the kernel had no room for it"),
        Failure::Unsupported => const_error!(Kind::Unsupported, "the object or its volume has no such operation"),
        Failure::Io => const_error!(Kind::Uncategorized, "a device failed, or what it holds does not decode"),
        Failure::BrokenPipe => const_error!(Kind::BrokenPipe, "the other end is gone"),
        Failure::Unknown => const_error!(Kind::Uncategorized, "the kernel refused in a word this std does not know"),
        Failure::BadDescriptor => const_error!(Kind::Uncategorized, "the descriptor is not open"),
        Failure::WrongDescriptor => const_error!(Kind::InvalidInput, "the descriptor names no object of that kind"),
        Failure::TooManyDescriptors => const_error!(Kind::QuotaExceeded, "the descriptor table is full"),
        Failure::NotUtf8 => const_error!(Kind::InvalidInput, "the path is not UTF-8"),
        Failure::Loop => const_error!(Kind::FilesystemLoop, "too many symbolic links"),
        Failure::CrossesDevices => const_error!(Kind::CrossesDevices, "the two paths are under different authorities"),
        Failure::NotADirectory => const_error!(Kind::NotADirectory, "not a directory"),
        Failure::IsADirectory => const_error!(Kind::IsADirectory, "is a directory"),
        Failure::NotEmpty => const_error!(Kind::DirectoryNotEmpty, "the directory is not empty"),
        Failure::NameTooLong => const_error!(Kind::InvalidFilename, "the name or path is too long"),
        Failure::InvalidName => const_error!(Kind::InvalidFilename, "the volume cannot store that name"),
        Failure::ReadOnly => const_error!(Kind::PermissionDenied, "the directory is read-only to this process"),
        Failure::NotOpenForWriting => const_error!(Kind::PermissionDenied, "the file is not open for writing"),
        Failure::Busy => const_error!(Kind::ResourceBusy, "it is in use where it is"),
        Failure::TextBusy => const_error!(Kind::ExecutableFileBusy, "a program runs from the file"),
        Failure::NoSpace => const_error!(Kind::StorageFull, "the volume is full"),
        Failure::FileTooLarge => const_error!(Kind::FileTooLarge, "the file would be too large"),
        Failure::Quota => const_error!(Kind::QuotaExceeded, "past a bound its server keeps"),
        Failure::TooManyOpenFiles => const_error!(Kind::QuotaExceeded, "too many files open on the file server"),
        Failure::Protocol => const_error!(Kind::InvalidData, "a server answered outside its protocol"),
        Failure::Stale => const_error!(
            Kind::StaleNetworkFileHandle,
            "the file was removed or replaced, or its file server restarted or ended, since it was opened",
        ),
        Failure::TimedOut => const_error!(Kind::TimedOut, "timed out"),
        Failure::ConnectionRefused => const_error!(Kind::ConnectionRefused, "connection refused"),
        Failure::ConnectionReset => const_error!(Kind::ConnectionReset, "connection reset"),
        Failure::ConnectionAborted => const_error!(Kind::ConnectionAborted, "netstack ended the connection"),
        Failure::NotConnected => const_error!(Kind::NotConnected, "not connected, or the machine has no network"),
        Failure::AddrInUse => const_error!(Kind::AddrInUse, "the address is in use"),
        Failure::AddrNotAvailable => const_error!(Kind::AddrNotAvailable, "this machine does not hold the address"),
        Failure::HostUnreachable => const_error!(Kind::HostUnreachable, "the peer is unreachable"),
        Failure::NetworkUnreachable => const_error!(Kind::NetworkUnreachable, "the peer's network is unreachable"),
        Failure::TryAgain => const_error!(Kind::ResourceBusy, "the lookup's lease changed under it: ask again"),
        Failure::NotExecutable => const_error!(Kind::InvalidData, "the file is no program this machine loads"),
        Failure::Truncated => const_error!(Kind::UnexpectedEof, "the file shrank while it was read"),
        Failure::TooBig => const_error!(Kind::InvalidInput, "more than a spawn carries"),
    }
}

/// `f` on the kernel handle descriptor `fd` names, held for the call
/// (`toyos::fd::with`): a descriptor that is not open answers EBADF's error,
/// and one a library keeps as no kernel handle is no file to a kernel call.
pub fn on_fd<T>(
    fd: i32,
    f: impl FnOnce(toyos::BorrowedHandle<'_>) -> Result<T, toyos_abi::syscall::SyscallError>,
) -> crate::io::Result<T> {
    let answer = toyos::fd::with(fd, |object| match object.kernel_handle() {
        Some(handle) => f(handle).map_err(failed),
        None => Err(crate::io::const_error!(
            crate::io::ErrorKind::Unsupported,
            "the descriptor names an object of a library's own",
        )),
    });
    answer.map_err(failed)?
}

/// Why nothing was spawned, as std's error: the refusal's kind, and its own
/// reason where it gives one.
pub fn refused(refused: toyos::spawn::Refused) -> crate::io::Error {
    use toyos::spawn::Refused;

    let error = failed(&refused);
    match refused {
        Refused::Permission(why) | Refused::Invalid(why) => crate::io::Error::new(error.kind(), why),
        Refused::NotStarted => {
            crate::io::Error::new(error.kind(), "the launcher started nothing it permits, or its answer was lost")
        }
        Refused::Stream(_) | Refused::Export(_) | Refused::File(_) | Refused::Syscall(_) => error,
    }
}

/// A kernel handle this process just got, as a descriptor of its own: std
/// creates descriptors not inherited, as std is close-on-exec on unix.
pub fn new_fd(handle: toyos::OwnedHandle) -> crate::io::Result<i32> {
    toyos::fd::install(toyos::fd::Object::handle(handle), false, 0).map_err(failed)
}

/// The descriptor table's refusal as std's error, by the name the fork's
/// `BorrowedFd::try_clone_to_owned` calls.
pub fn fd_error(e: toyos::fd::Error) -> crate::io::Error {
    failed(e)
}

// The entry, on an empty stack. The image relocates itself first, from its
// own header's address.
#[cfg(target_arch = "x86_64")]
#[unsafe(no_mangle)]
#[unsafe(naked)]
unsafe extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "lea rdi, [rip + __ehdr_start]",
        "call {relocate}",
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

extern "C" fn start_rust() -> ! {
    unsafe extern "C" {
        fn main(argc: i32, argv: *const *const u8) -> i32;
        static __ehdr_start: u8;
    }
    let (argc, argv) = toyos::startup::args();
    // The program goes by its `argv[0]`.
    let name = match argc {
        0 => &[][..],
        _ => unsafe { crate::ffi::CStr::from_ptr((*argv).cast()) }.to_bytes(),
    };
    // SAFETY: the executable's own header, and this is its main thread.
    unsafe { toyos::tls::start(&raw const __ehdr_start, name) };

    eh_frame::init();
    crate::sys::env::init();
    // Before anything that names a symbol of a library it links.
    // SAFETY: the executable's own header, its thread-local storage laid out,
    // and this its main thread.
    unsafe { toyos::dl::start(&raw const __ehdr_start) };

    let code = unsafe { main(argc as i32, argv) };
    crate::sys::stdio::finish();
    toyos_abi::syscall::exit(code)
}

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
