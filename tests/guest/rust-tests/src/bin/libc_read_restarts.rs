//! libc's `read` interrupted by a `SIGCHLD` handler: restarted when it was
//! installed with `SA_RESTART`, answering `EINTR` when not (stage 4 of
//! `issues/a-childs-end-is-an-event-and-a-parent-takes-its-children-down.md`).
//!
//! **A Rust program because the order has to be the kernel's.** The child
//! must end while the read blocks, and only the kernel's roster says a thread
//! is parked, which a C program has no call to read; the calls under test are
//! libc's own, made through its C interface as a C program makes them. A
//! thread of this program's waits until the main thread is parked in the read,
//! then lets the child end, and writes the read's byte once the handler has
//! said it ran.
//!
//! **A quit's interrupt, the same way**: a child of this program handles
//! `SIGINT` without `SA_RESTART` and reads its empty standard input, and the
//! parent sends the interrupt once the roster shows that child's main thread
//! parked, which after its `armed` is the read. The read answers `EINTR`. The
//! parent writes the child a byte once its handler has said it ran, so a read
//! the interrupt did not end answers that byte and fails by name.

use std::fs::File;
use std::io::Read;
use std::os::toyos::io::FromRawFd;

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::syscap::SysCap;

#[path = "../roster.rs"]
mod roster;

const SELF_PATH: &str = "/system/bin/test_rs_libc_read_restarts";

const SIGINT: i32 = 2;
const SIGCHLD: i32 = 17;
const SA_RESTART: u64 = 0x1000_0000;
const EINTR: i32 = 4;

#[repr(C)]
struct SigAction {
    handler: usize,
    flags: u64,
    restorer: usize,
    mask: u64,
}

#[repr(C)]
struct FileActions {
    actions: usize,
    reserved: [usize; 7],
}

// libc's calls, which a Rust program links only where it asks for them.
#[link(name = "toyos_c", kind = "static")]
unsafe extern "C" {
    fn sigaction(sig: i32, act: *const SigAction, old: *mut SigAction) -> i32;
    fn read(fd: i32, buf: *mut u8, count: usize) -> isize;
    fn write(fd: i32, buf: *const u8, count: usize) -> isize;
    fn pipe(fds: *mut i32) -> i32;
    fn close(fd: i32) -> i32;
    fn posix_spawn_file_actions_init(fa: *mut FileActions) -> i32;
    fn posix_spawn_file_actions_adddup2(fa: *mut FileActions, fd: i32, newfd: i32) -> i32;
    fn posix_spawn_file_actions_destroy(fa: *mut FileActions) -> i32;
    fn posix_spawn(
        pid: *mut i32,
        path: *const u8,
        fa: *const FileActions,
        attr: *const u8,
        argv: *const *const u8,
        envp: *const *const u8,
    ) -> i32;
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
    fn kill(pid: i32, sig: i32) -> i32;
    fn __errno_location() -> *mut i32;
}

/// Where the handler says it ran.
static mut RAN: i32 = -1;

extern "C" fn on_child(_: i32) {
    // SAFETY: `RAN` is written once, before any child is started.
    unsafe { write(RAN, b"h".as_ptr(), 1) };
}

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("held") => {
            let _ = std::io::stdin().read_to_end(&mut Vec::new());
        }
        Some("interrupted") => interrupted(),
        Some(other) => panic!("unknown role {other:?}"),
        None => test(),
    }
}

fn test() {
    let ran = pipe_pair();
    // SAFETY: no child, so no handler, runs yet.
    unsafe { RAN = ran[1] };
    let handler_said = ran[0];
    read_across_the_handler(SA_RESTART, handler_said, true);
    read_across_the_handler(0, handler_said, false);
    println!("libc_read_restarts: a read across a SIGCHLD handler restarted with SA_RESTART and answered EINTR without");
    read_across_an_interrupt();
    println!("libc_read_restarts: a child's read across an interrupt, SIGINT handled without SA_RESTART, answered EINTR");
}

/// What the interrupted child says once its handler is kept.
const ARMED: &[u8] = b"armed\n";

/// How often the child's `SIGINT` handler ran.
static TAKEN: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

extern "C" fn on_interrupt(_: i32) {
    TAKEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // SAFETY: a byte of a constant, to this process's standard output.
    unsafe { write(1, b"h".as_ptr(), 1) };
}

/// The child: `SIGINT` handled without `SA_RESTART`, [`ARMED`] said, then a
/// read of its standard input, empty until its handler has said it ran,
/// which the interrupt ends with `EINTR`.
fn interrupted() {
    let action = SigAction { handler: on_interrupt as extern "C" fn(i32) as usize, flags: 0, restorer: 0, mask: 0 };
    // SAFETY: a `struct sigaction` as `<signal.h>` lays it out.
    assert_eq!(unsafe { sigaction(SIGINT, &action, std::ptr::null_mut()) }, 0, "sigaction");
    // SAFETY: the bytes of a constant, to this process's standard output.
    assert_eq!(unsafe { write(1, ARMED.as_ptr(), ARMED.len()) }, ARMED.len() as isize, "armed");
    let mut byte = [0u8];
    // SAFETY: a one-byte buffer of this frame.
    let got = unsafe { read(0, byte.as_mut_ptr(), 1) };
    // SAFETY: the calling thread's own `errno`.
    let errno = unsafe { *__errno_location() };
    let taken = TAKEN.load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        got == -1 && errno == EINTR && taken == 1,
        "libc_read_restarts: an interrupt did not end the child's read with EINTR: it answered {got} (errno {errno}), the handler run {taken} times"
    );
}

/// The interrupted child, sent `SIGINT` once it has said [`ARMED`] and the
/// roster shows its main thread parked: it ends 0, its read having answered
/// `EINTR`. A byte follows the handler's word, which only a read still
/// parked takes.
fn read_across_an_interrupt() {
    // The default again: the child's end interrupts no call of this parent's.
    let default = SigAction { handler: 0, flags: 0, restorer: 0, mask: 0 };
    // SAFETY: a `struct sigaction` as `<signal.h>` lays it out.
    assert_eq!(unsafe { sigaction(SIGCHLD, &default, std::ptr::null_mut()) }, 0, "sigaction");
    let (stdin, said) = (pipe_pair(), pipe_pair());
    let child = spawn_role("interrupted", stdin[0], Some(said[1]));
    for fd in [stdin[0], said[1]] {
        // SAFETY: this program's descriptors, the child's now, not used again here.
        unsafe { close(fd) };
    }
    let mut armed = [0u8; ARMED.len()];
    let mut have = 0;
    while have < armed.len() {
        // SAFETY: the unfilled rest of a buffer of this frame.
        let got = unsafe { read(said[0], armed[have..].as_mut_ptr(), armed.len() - have) };
        assert!(got > 0, "the child ended before it was armed (read answered {got})");
        have += got as usize;
    }
    assert_eq!(armed, ARMED, "the child said something else than armed");
    roster::await_true(|| roster::threads_of(cap(), child as u32).iter().any(|&(is_thread, state)| !is_thread && state == roster::BLOCKED));
    // SAFETY: a child of this process, by the pid its spawn answered.
    assert_eq!(unsafe { kill(child, SIGINT) }, 0, "kill(SIGINT)");
    let mut ran = [0u8];
    // SAFETY: a one-byte buffer of this frame.
    let got = unsafe { read(said[0], ran.as_mut_ptr(), 1) };
    assert!(got == 1 && ran == *b"h", "the child's SIGINT handler never said it ran (read answered {got})");
    // Refused once the child has ended, which an interrupted read lets it.
    // SAFETY: the child's stdin's write end, open until this test closes it.
    unsafe { write(stdin[1], b"b".as_ptr(), 1) };
    let mut status = 0;
    // SAFETY: a status word of this frame.
    assert_eq!(unsafe { waitpid(child, &mut status, 0) }, child, "waitpid");
    assert_eq!(status, 0, "the interrupted child ended {status:#x}: its read was not ended by the interrupt");
    for fd in [stdin[1], said[0]] {
        // SAFETY: this program's descriptors, not used again.
        unsafe { close(fd) };
    }
}

/// A read of an empty pipe, a child ending while it blocks, the handler for
/// `SIGCHLD` installed with `flags`, and a byte written into the pipe once the
/// handler ran: the read answers the byte where it `restarts`, and `EINTR`
/// where not.
fn read_across_the_handler(flags: u64, handler_said: i32, restarts: bool) {
    let action = SigAction { handler: on_child as extern "C" fn(i32) as usize, flags, restorer: 0, mask: 0 };
    // SAFETY: a `struct sigaction` as `<signal.h>` lays it out.
    assert_eq!(unsafe { sigaction(SIGCHLD, &action, std::ptr::null_mut()) }, 0, "sigaction");
    let read_from = pipe_pair();
    let stdin = pipe_pair();
    let child = spawn_role("held", stdin[0], None);
    // SAFETY: this program's descriptor, not used again here.
    unsafe { close(stdin[0]) };

    let (let_go, write_to) = (stdin[1], read_from[1]);
    let helper = std::thread::spawn(move || {
        roster::await_true(main_thread_is_parked);
        // SAFETY: the child's stdin's write end, which its end follows.
        unsafe { close(let_go) };
        // std's read, so this thread is in no call of libc's for the handler to interrupt.
        // SAFETY: the read end of libc's pipe, held by this thread alone from here.
        let mut said = unsafe { File::from_raw_fd(handler_said) };
        let mut byte = [0u8];
        said.read_exact(&mut byte).expect("the handler's byte");
        std::mem::forget(said);
        // SAFETY: the pipe's write end, open until the test closes it.
        unsafe { write(write_to, b"b".as_ptr(), 1) };
    });

    let mut byte = [0u8];
    // SAFETY: a one-byte buffer of this frame.
    let got = unsafe { read(read_from[0], byte.as_mut_ptr(), 1) };
    // SAFETY: the calling thread's own `errno`.
    let errno = unsafe { *__errno_location() };
    helper.join().expect("the helper thread");
    match restarts {
        true => assert!(got == 1 && byte == *b"b", "with SA_RESTART the read answered {got} (errno {errno}), not the byte"),
        false => assert!(got == -1 && errno == EINTR, "without SA_RESTART the read answered {got} (errno {errno}), not EINTR"),
    }
    let mut status = 0;
    // SAFETY: a status word of this frame.
    assert_eq!(unsafe { waitpid(child, &mut status, 0) }, child, "waitpid");
    for fd in read_from {
        // SAFETY: this program's descriptors, not used again.
        unsafe { close(fd) };
    }
}

fn pipe_pair() -> [i32; 2] {
    let mut fds = [0i32; 2];
    // SAFETY: two descriptors' room.
    assert_eq!(unsafe { pipe(fds.as_mut_ptr()) }, 0, "pipe");
    fds
}

/// This binary as `role`, with `stdin` as its standard input and `stdout`,
/// where given, as its standard output: its pid.
fn spawn_role(role: &str, stdin: i32, stdout: Option<i32>) -> i32 {
    let path = std::ffi::CString::new(SELF_PATH).unwrap();
    let role = std::ffi::CString::new(role).unwrap();
    let argv = [path.as_ptr().cast::<u8>(), role.as_ptr().cast::<u8>(), std::ptr::null()];
    let envp = [std::ptr::null::<u8>()];
    let mut actions = FileActions { actions: 0, reserved: [0; 7] };
    let mut pid = 0;
    // SAFETY: the file actions as `<spawn.h>` declares them, made and
    // destroyed here; `argv` and `envp` null-terminated arrays of C strings.
    unsafe {
        assert_eq!(posix_spawn_file_actions_init(&mut actions), 0);
        assert_eq!(posix_spawn_file_actions_adddup2(&mut actions, stdin, 0), 0);
        if let Some(stdout) = stdout {
            assert_eq!(posix_spawn_file_actions_adddup2(&mut actions, stdout, 1), 0);
        }
        assert_eq!(posix_spawn(&mut pid, path.as_ptr().cast(), &actions, std::ptr::null(), argv.as_ptr(), envp.as_ptr()), 0, "posix_spawn");
        posix_spawn_file_actions_destroy(&mut actions);
    }
    pid
}

fn main_thread_is_parked() -> bool {
    roster::my_threads(cap()).iter().any(|&(is_thread, state)| !is_thread && state == roster::BLOCKED)
}

fn cap() -> &'static SysCap {
    static CAP: std::sync::OnceLock<SysCap> = std::sync::OnceLock::new();
    CAP.get_or_init(|| Endowments::get().take(SYSCAP_LABEL).expect("test-runner endows every binary it spawns a system capability"))
}
