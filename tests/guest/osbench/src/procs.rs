//! Processes and threads in more depth than `system`'s: what a program's
//! size costs its spawn, C's `posix_spawn` beside std's `Command`, a
//! spawn-and-wait loop, and a thread's stack size.
//!
//! **The tiny and large programs are written here**, the same bytes on both
//! systems but the exit call each kernel takes: a position-independent ELF of
//! one `PT_LOAD` holding the code (under 300 bytes), and the same with a
//! second, read-only `PT_LOAD` of [`LARGE`] bytes of file data the program
//! never touches. They live in the directory `files` measures in memory
//! (ToyOS's `/tmp`, Linux's `/dev/shm`), so no disk is read.

use std::ffi::{c_char, c_int, c_void, CString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

use crate::files::Scratch;
use crate::measure::{each, io};
use crate::report::refuse;
use crate::sys;

/// The large program's untouched data: 32 MiB.
const LARGE: usize = 32 << 20;

const PT_LOAD: u32 = 1;
const PT_GNU_STACK: u32 = 0x6474_e551;
const EHDR: usize = 64;
const PHDR: usize = 56;

/// A position-independent executable for this machine that exits 0 at once, with
/// `payload` bytes of read-only data beside its code where `payload` is not
/// zero. Layout per the System V gABI and its x86-64 supplement: the header,
/// the program headers, the code, all in one R+X `PT_LOAD` from offset 0;
/// the payload in a second R `PT_LOAD` at file offset 4 KiB and address
/// 2 MiB; a `PT_GNU_STACK` saying the stack is not executable.
pub fn program(payload: usize) -> Vec<u8> {
    let phnum = if payload > 0 { 3 } else { 2 };
    let code_at = EHDR + PHDR * phnum;
    let first = code_at + crate::arch::EXIT_CODE.len();
    let mut out = vec![0u8; first];
    let put = |out: &mut Vec<u8>, at: usize, bytes: &[u8]| out[at..at + bytes.len()].copy_from_slice(bytes);
    put(&mut out, 0, b"\x7fELF\x02\x01\x01");
    put(&mut out, 16, &3u16.to_le_bytes()); // ET_DYN
    put(&mut out, 18, &crate::arch::ELF_MACHINE.to_le_bytes());
    put(&mut out, 20, &1u32.to_le_bytes()); // EV_CURRENT
    put(&mut out, 24, &(code_at as u64).to_le_bytes()); // e_entry
    put(&mut out, 32, &(EHDR as u64).to_le_bytes()); // e_phoff
    put(&mut out, 52, &(EHDR as u16).to_le_bytes()); // e_ehsize
    put(&mut out, 54, &(PHDR as u16).to_le_bytes()); // e_phentsize
    put(&mut out, 56, &(phnum as u16).to_le_bytes()); // e_phnum
    put(&mut out, 58, &64u16.to_le_bytes()); // e_shentsize, with no sections
    let header = |out: &mut Vec<u8>, i: usize, kind: u32, flags: u32, offset: u64, vaddr: u64, size: u64| {
        let at = EHDR + PHDR * i;
        put(out, at, &kind.to_le_bytes());
        put(out, at + 4, &flags.to_le_bytes());
        put(out, at + 8, &offset.to_le_bytes());
        put(out, at + 16, &vaddr.to_le_bytes());
        put(out, at + 24, &vaddr.to_le_bytes());
        put(out, at + 32, &size.to_le_bytes());
        put(out, at + 40, &size.to_le_bytes());
        put(out, at + 48, &(if kind == PT_LOAD { 0x1000u64 } else { 16 }).to_le_bytes());
    };
    header(&mut out, 0, PT_LOAD, 0b101, 0, 0, first as u64);
    header(&mut out, 1, PT_GNU_STACK, 0b110, 0, 0, 0);
    if payload > 0 {
        header(&mut out, 2, PT_LOAD, 0b100, 0x1000, 0x20_0000, payload as u64);
    }
    put(&mut out, code_at, &crate::arch::EXIT_CODE);
    if payload > 0 {
        out.resize(0x1000, 0);
        out.extend((0..payload).map(|i| (i % 249) as u8 | 1));
    }
    out
}

fn write_program(dir: &Scratch, name: &str, payload: usize) -> std::io::Result<PathBuf> {
    let path = dir.at(name);
    fs::write(&path, program(payload))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
    }
    Ok(path)
}

/// A generated program refused for its directory's mount: Linux answers a
/// `noexec` mount with `EACCES`, and that alone is an absence; ToyOS has none.
fn refused(why: String, permission_denied: bool) -> sys::Refused {
    if cfg!(target_os = "linux") && permission_denied {
        sys::Refused::Absent(format!("the directory's mount runs no program: {why}"))
    } else {
        sys::Refused::Failed(why)
    }
}

fn spawn_wait(path: &Path) -> Result<(), sys::Refused> {
    let status = Command::new(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| refused(format!("spawn {}: {e}", path.display()), e.kind() == std::io::ErrorKind::PermissionDenied))?;
    if status.success() { Ok(()) } else { Err(sys::Refused::Failed(format!("{} ended {status}", path.display()))) }
}

// ToyOS's C library is no part of std there; a Rust program calling it links
// the sysroot's archive of it, built for linking beside std
// (`build/libc.rs`). Linux's is musl, which std already links.
#[cfg_attr(target_os = "toyos", link(name = "toyos_c", kind = "static"))]
extern "C" {
    fn posix_spawn(
        pid: *mut i32,
        path: *const c_char,
        file_actions: *const c_void,
        attributes: *const c_void,
        argv: *const *const c_char,
        envp: *const *const c_char,
    ) -> c_int;
    fn waitpid(pid: i32, status: *mut c_int, options: c_int) -> i32;
}

/// C's `posix_spawn` with no file actions and no attributes, then `waitpid`:
/// the child inherits this process's descriptors as they are.
fn c_spawn_wait(path: &CString, argv: &[&CString]) -> Result<(), sys::Refused> {
    let mut args: Vec<*const c_char> = argv.iter().map(|a| a.as_ptr()).collect();
    args.push(std::ptr::null());
    let envp: [*const c_char; 1] = [std::ptr::null()];
    let mut pid = 0i32;
    // SAFETY: every pointer names a NUL-terminated string or the null that
    // ends its array, live across the call.
    let err = unsafe { posix_spawn(&mut pid, path.as_ptr(), std::ptr::null(), std::ptr::null(), args.as_ptr(), envp.as_ptr()) };
    if err != 0 {
        const EACCES: c_int = 13;
        return Err(refused(format!("posix_spawn answered {err}"), err == EACCES));
    }
    let mut status = 0;
    // SAFETY: `status` is a valid out-pointer and `pid` this process's child.
    let reaped = unsafe { waitpid(pid, &mut status, 0) };
    if reaped != pid {
        return Err(sys::Refused::Failed(format!("waitpid answered {reaped}: {}", std::io::Error::last_os_error())));
    }
    if status != 0 { Err(sys::Refused::Failed(format!("the child's wait status was {status:#x}"))) } else { Ok(()) }
}

pub fn suite() {
    let memory = crate::files::memory().dir;
    let dir = match Scratch::new(&memory) {
        Ok(d) => d,
        Err(e) => return refuse("proc", format!("no scratch directory under {memory}: {e}")),
    };
    let (tiny, large) = match (write_program(&dir, "tiny", 0), write_program(&dir, "large", LARGE)) {
        (Ok(t), Ok(l)) => (t, l),
        (Err(e), _) | (_, Err(e)) => return refuse("proc", format!("the generated programs: {e}")),
    };
    let size = |p: &Path| fs::metadata(p).map(|m| m.len().to_string()).unwrap_or_default();

    // The tiny program spawned by `Command` and waited for: 200 after 20.
    let spawn = |p: &Path| spawn_wait(p).map_err(sys::Refused::why);
    if let Err(refused) = spawn_wait(&tiny) {
        refused.report("proc.spawn_wait.tiny");
    } else {
        each("proc.spawn_wait.tiny", 20, 200, &[("file_bytes", size(&tiny))], |_| spawn(&tiny));

        // The same program with 32 MiB of data it never reads: 100 after 10.
        each("proc.spawn_wait.large", 10, 100, &[("file_bytes", size(&large))], |_| spawn(&large));

        // A hundred spawn-and-waits back to back, ns per hundred: 10 after 1.
        each("proc.spawn_wait_loop.100", 1, 10, &[("spawns", "100".into())], |_| {
            (0..100).try_for_each(|_| spawn(&tiny))
        });

        // C's `posix_spawn` and `waitpid` of the tiny program: 200 after 20.
        let c_tiny = CString::new(tiny.to_string_lossy().as_bytes()).expect("a path without NUL");
        match c_spawn_wait(&c_tiny, &[&c_tiny]) {
            Ok(()) => each("proc.posix_spawn_wait.tiny", 20, 200, &[], |_| {
                c_spawn_wait(&c_tiny, &[&c_tiny]).map_err(sys::Refused::why)
            }),
            Err(refused) => refused.report("proc.posix_spawn_wait.tiny"),
        }
    }

    // This program, std and all, as `osbench nop`, by C's `posix_spawn`, to
    // set beside `process.spawn_wait`, std's `Command` of the same: 100 after 10.
    match std::env::current_exe() {
        Ok(exe) => {
            let path = CString::new(exe.to_string_lossy().as_bytes()).expect("a path without NUL");
            let nop = CString::new("nop").expect("no NUL");
            match c_spawn_wait(&path, &[&path, &nop]) {
                Ok(()) => each("proc.posix_spawn_wait.std", 10, 100, &[("file_bytes", size(&exe))], |_| {
                    c_spawn_wait(&path, &[&path, &nop]).map_err(sys::Refused::why)
                }),
                Err(refused) => refused.report("proc.posix_spawn_wait.std"),
            }
        }
        Err(e) => refuse("proc.posix_spawn_wait.std", e),
    }

    // `thread::spawn` and `join` of an empty closure with std's default stack
    // and with a 64 KiB one asked for: 1,000 after 50 each.
    for (name, stack) in [("default", None), ("stack64k", Some(64 << 10))] {
        let asked = stack.map_or("default".to_string(), |s: usize| s.to_string());
        each(
            &format!("thread.spawn_join.{name}"),
            50,
            1000,
            &[("stack_asked", asked), ("stack_floor", sys::STACK_FLOOR.into())],
            |_| {
                let builder = match stack {
                    Some(bytes) => thread::Builder::new().stack_size(bytes),
                    None => thread::Builder::new(),
                };
                let handle = io("spawn", builder.spawn(|| {}))?;
                handle.join().map_err(|_| "an empty thread panicked".to_string())
            },
        );
    }
}
