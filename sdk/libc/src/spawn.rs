//! Children: `posix_spawn` and `posix_spawnp` with their file actions and
//! attributes, the wait family, `kill`, and `system`, `popen` and `pclose`
//! through `/system/bin/shell -c`.
//!
//! **A child is the `Process` handle its spawn answered**, which this library
//! keeps under its pid until it is waited for; a pid names nothing else here,
//! and `kill` and `waitpid` reach only this process's own children. A child
//! starts by `toyos::spawn`'s rule, as std's do: launched where the
//! supervisor declares it, spawned here otherwise. It starts with descriptors
//! 0–2 and exactly what its file actions name (`spawnreq`). A child's group is
//! a number this library keeps, which `kill(-pgid, …)` and `waitpid(-pgid,
//! …)` read; the kernel has none.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ptr;
use core::sync::atomic::{AtomicUsize, Ordering};

use toyos::process::{End, Process};
use toyos::spawn::{self, Launched, Parent, Refused, Spawn};
use toyos_abi::syscall;
use toyos_abi::RawHandle;

use crate::errcode::{E2BIG, EACCES, EAGAIN, EBADF, ECHILD, EINVAL, ENOENT, EPERM, ESRCH};
use crate::pthread::Lock;
use crate::spawnreq::{self, Action};

const SIGKILL: i32 = 9;
const WNOHANG: i32 = 1;
const WUNTRACED: i32 = 2;
const WCONTINUED: i32 = 8;

const POSIX_SPAWN_RESETIDS: i16 = 0x01;
const POSIX_SPAWN_SETPGROUP: i16 = 0x02;
const POSIX_SPAWN_SETSIGDEF: i16 = 0x04;
const POSIX_SPAWN_SETSIGMASK: i16 = 0x08;
/// The flags a spawn honours. No signal but `SIGKILL` reaches a child yet,
/// and that one no mask holds, so a child's mask and defaults change nothing
/// it can see; and a process has no ids to reset.
const FLAGS: i16 = POSIX_SPAWN_RESETIDS | POSIX_SPAWN_SETPGROUP | POSIX_SPAWN_SETSIGDEF | POSIX_SPAWN_SETSIGMASK;

/// The most children unwaited at once, the track's bound: one poller watches
/// them all, and its 256th watch is stage 6's notice
/// (`issues/a-childs-end-is-an-event-and-a-parent-takes-its-children-down.md`).
const MAX_CHILDREN: usize = toyos::poller::Poller::MAX_HANDLES as usize - 1;

/// What the shell `system` and `popen` run a command with.
const SHELL: &str = "/system/bin/shell";

struct Child {
    pid: i32,
    group: i32,
    process: Process,
}

/// Every child not yet waited for, in the order they were spawned.
static CHILDREN: Lock<Vec<Child>> = Lock::new(Vec::new());
/// Children spawned or being spawned and not yet waited for, against
/// [`MAX_CHILDREN`].
static UNWAITED: AtomicUsize = AtomicUsize::new(0);

/// `<spawn.h>`'s `posix_spawn_file_actions_t`: the actions, boxed.
#[repr(C)]
pub struct FileActions {
    actions: *mut Vec<Action<Box<[u8]>>>,
    _reserved: [usize; 7],
}

/// `<spawn.h>`'s `posix_spawnattr_t`.
#[repr(C)]
pub struct SpawnAttr {
    flags: i16,
    pgroup: i32,
    mask: u64,
    default: u64,
    _reserved: [usize; 4],
}

#[no_mangle]
pub unsafe extern "C" fn posix_spawn_file_actions_init(fa: *mut FileActions) -> i32 {
    unsafe { fa.write(FileActions { actions: Box::into_raw(Box::new(Vec::new())), _reserved: [0; 7] }) };
    0
}

#[no_mangle]
pub unsafe extern "C" fn posix_spawn_file_actions_destroy(fa: *mut FileActions) -> i32 {
    // SAFETY: a `FileActions` `init` made, destroyed once.
    drop(unsafe { Box::from_raw((*fa).actions) });
    0
}

/// # Safety
/// `fa` is a `FileActions` `init` made and not destroyed.
unsafe fn add(fa: *mut FileActions, action: Action<Box<[u8]>>) -> i32 {
    unsafe { (*(*fa).actions).push(action) };
    0
}

#[no_mangle]
pub unsafe extern "C" fn posix_spawn_file_actions_addopen(
    fa: *mut FileActions,
    fd: i32,
    path: *const u8,
    oflag: i32,
    mode: u32,
) -> i32 {
    let Ok(fd) = spawnreq::child_fd(fd) else { return EBADF };
    let path: Box<[u8]> = crate::posix_io::c_str_to_bytes(path).into();
    unsafe { add(fa, Action::Open { fd, path, oflag, mode }) }
}

#[no_mangle]
pub unsafe extern "C" fn posix_spawn_file_actions_addclose(fa: *mut FileActions, fd: i32) -> i32 {
    match spawnreq::any_fd(fd) {
        Ok(fd) => unsafe { add(fa, Action::Close { fd }) },
        Err(e) => e,
    }
}

#[no_mangle]
pub unsafe extern "C" fn posix_spawn_file_actions_adddup2(fa: *mut FileActions, fd: i32, newfd: i32) -> i32 {
    match (spawnreq::any_fd(fd), spawnreq::child_fd(newfd)) {
        (Ok(fd), Ok(newfd)) => unsafe { add(fa, Action::Dup2 { fd, newfd }) },
        _ => EBADF,
    }
}

#[no_mangle]
pub unsafe extern "C" fn posix_spawnattr_init(attr: *mut SpawnAttr) -> i32 {
    unsafe { attr.write(SpawnAttr { flags: 0, pgroup: 0, mask: 0, default: 0, _reserved: [0; 4] }) };
    0
}

#[no_mangle]
pub unsafe extern "C" fn posix_spawnattr_destroy(_attr: *mut SpawnAttr) -> i32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn posix_spawnattr_setflags(attr: *mut SpawnAttr, flags: i16) -> i32 {
    if flags & !FLAGS != 0 {
        return EINVAL;
    }
    unsafe { (*attr).flags = flags };
    0
}

#[no_mangle]
pub unsafe extern "C" fn posix_spawnattr_getflags(attr: *const SpawnAttr, flags: *mut i16) -> i32 {
    unsafe { *flags = (*attr).flags };
    0
}

#[no_mangle]
pub unsafe extern "C" fn posix_spawnattr_setpgroup(attr: *mut SpawnAttr, pgroup: i32) -> i32 {
    unsafe { (*attr).pgroup = pgroup };
    0
}

#[no_mangle]
pub unsafe extern "C" fn posix_spawnattr_getpgroup(attr: *const SpawnAttr, pgroup: *mut i32) -> i32 {
    unsafe { *pgroup = (*attr).pgroup };
    0
}

#[no_mangle]
pub unsafe extern "C" fn posix_spawnattr_setsigmask(attr: *mut SpawnAttr, mask: *const u64) -> i32 {
    unsafe { (*attr).mask = *mask };
    0
}

#[no_mangle]
pub unsafe extern "C" fn posix_spawnattr_getsigmask(attr: *const SpawnAttr, mask: *mut u64) -> i32 {
    unsafe { *mask = (*attr).mask };
    0
}

#[no_mangle]
pub unsafe extern "C" fn posix_spawnattr_setsigdefault(attr: *mut SpawnAttr, set: *const u64) -> i32 {
    unsafe { (*attr).default = *set };
    0
}

#[no_mangle]
pub unsafe extern "C" fn posix_spawnattr_getsigdefault(attr: *const SpawnAttr, set: *mut u64) -> i32 {
    unsafe { *set = (*attr).default };
    0
}

/// `strs`, a null-terminated array of C strings, as `SYS_SPAWN`'s blob of
/// them, each ended by `NUL`.
///
/// # Safety
/// `strs` is null or a null-terminated array of C strings.
unsafe fn blob(strs: *const *const u8) -> Vec<u8> {
    let mut out = Vec::new();
    let mut at = strs;
    while !at.is_null() && !unsafe { *at }.is_null() {
        out.extend_from_slice(crate::posix_io::c_str_to_bytes(unsafe { *at }));
        out.push(0);
        at = unsafe { at.add(1) };
    }
    out
}

#[no_mangle]
pub unsafe extern "C" fn posix_spawn(
    pid: *mut i32,
    path: *const u8,
    fa: *const FileActions,
    attr: *const SpawnAttr,
    argv: *const *const u8,
    envp: *const *const u8,
) -> i32 {
    let Ok(path) = core::str::from_utf8(crate::posix_io::c_str_to_bytes(path)) else { return EINVAL };
    let actions: &[Action<Box<[u8]>>] = match unsafe { fa.as_ref() } {
        Some(fa) => unsafe { &*fa.actions },
        None => &[],
    };
    let group = match unsafe { attr.as_ref() } {
        Some(attr) if attr.flags & POSIX_SPAWN_SETPGROUP != 0 => Some(attr.pgroup),
        _ => None,
    };
    let argv = unsafe { blob(argv) };
    let env = match envp.is_null() {
        true => crate::env::blob(),
        false => unsafe { blob(envp) },
    };
    match spawn_child(path, &argv, &env, actions, group) {
        Ok(child) => {
            if let Some(pid) = unsafe { pid.as_mut() } {
                *pid = child;
            }
            0
        }
        Err(errno) => errno,
    }
}

#[no_mangle]
pub unsafe extern "C" fn posix_spawnp(
    pid: *mut i32,
    file: *const u8,
    fa: *const FileActions,
    attr: *const SpawnAttr,
    argv: *const *const u8,
    envp: *const *const u8,
) -> i32 {
    let name = crate::posix_io::c_str_to_bytes(file);
    if name.contains(&b'/') {
        return unsafe { posix_spawn(pid, file, fa, attr, argv, envp) };
    }
    match crate::env::find_in_path(name) {
        Some(found) => unsafe { posix_spawn(pid, found.as_ptr(), fa, attr, argv, envp) },
        None => ENOENT,
    }
}

/// Start `path` with `argv` and `env`, its descriptors as `actions` leave
/// them, in `group` or this process's: its pid, or an `errno`.
fn spawn_child(path: &str, argv: &[u8], env: &[u8], actions: &[Action<Box<[u8]>>], group: Option<i32>) -> Result<i32, i32> {
    if UNWAITED.fetch_add(1, Ordering::Relaxed) >= MAX_CHILDREN {
        UNWAITED.fetch_sub(1, Ordering::Relaxed);
        return Err(EAGAIN);
    }
    let spawned = spawn_counted(path, argv, env, actions, group);
    if spawned.is_err() {
        UNWAITED.fetch_sub(1, Ordering::Relaxed);
    }
    spawned
}

/// [`spawn_child`], its place in [`UNWAITED`] taken.
fn spawn_counted(path: &str, argv: &[u8], env: &[u8], actions: &[Action<Box<[u8]>>], group: Option<i32>) -> Result<i32, i32> {
    // Copies of what this process's descriptors name, closed once the child
    // holds its own.
    let mut held: Vec<toyos::OwnedHandle> = Vec::new();
    let plan = spawnreq::plan(
        |fd| {
            let Some(copy) = toyos::fd::copy(fd as i32).map_err(errno_of)? else { return Ok(None) };
            let Ok(inheritable) = toyos::fd::inheritable(fd as i32) else { return Ok(None) };
            let handle = toyos::AsHandle::as_handle(&copy).0;
            held.push(copy);
            Ok(Some((handle, !inheritable)))
        },
        actions,
        |path, oflag, mode| crate::posix_io::open_handle(path, oflag, mode).map(|h| h.0),
        |handle| syscall::close(RawHandle(handle)),
    )?;
    let started = start(path, argv, env, &plan.fds);
    for handle in plan.opened {
        syscall::close(RawHandle(handle));
    }
    drop(held);
    let process = started?;
    // Asked of a child this process just started and holds, which answers.
    let pid = process.stats().expect("posix_spawn: a child's accounting").pid as i32;
    let group = match group {
        Some(0) => pid,
        Some(group) => group,
        None => own_group(),
    };
    CHILDREN.lock().push(Child { pid, group, process });
    Ok(pid)
}

/// The launch-or-spawn of `toyos::spawn`, from this process's own working
/// directory, the child's `HOME` the one `env` names if it differs from this
/// process's start-up one.
fn start(path: &str, argv: &[u8], env: &[u8], fds: &[(u32, u32)]) -> Result<Process, i32> {
    let mut cwd = alloc::vec![0u8; syscall::MAX_PATH];
    let n = syscall::getcwd(&mut cwd);
    let cwd = core::str::from_utf8(&cwd[..n]).map_err(|_| EINVAL)?;
    let fds: Vec<(u32, RawHandle)> = fds.iter().map(|&(fd, handle)| (fd, RawHandle(handle))).collect();
    let request = Spawn {
        program: path,
        image: None,
        argv,
        env,
        cwd,
        fds: &fds,
        endow: &[],
        provided: &[],
        parent: Parent::Caller,
    };
    let mut answer = alloc::vec![0u8; toyos::ipc::MAX_FRAME_LEN as usize];
    let home = match request.launch(&mut answer).map_err(errno_of)? {
        Launched::Started(process) => return Ok(process),
        Launched::Direct { home } => home,
    };
    let mut direct = Vec::new();
    spawn::direct_env(env, crate::env::names_home(env), home, &mut direct);
    request.direct(&direct, None).map_err(errno_of)
}

fn errno_of(refused: Refused) -> i32 {
    match refused {
        Refused::Permission(_) | Refused::NotStarted => EACCES,
        Refused::Invalid(_) => E2BIG,
        Refused::Stream(_) => EBADF,
        Refused::Syscall(e) => crate::errcode::of(e),
    }
}

/// This process's group: its pid, which the children it spawns share unless
/// their spawn names another.
fn own_group() -> i32 {
    syscall::getpid().0 as i32
}

#[no_mangle]
pub unsafe extern "C" fn getpgrp() -> i32 {
    own_group()
}

/// The children `pid` names to `waitpid` and `kill`: one by its pid, every
/// child for -1, this process's group for 0, and group `-pid` below -1.
fn names(pid: i32, child: &Child) -> bool {
    match pid {
        -1 => true,
        0 => child.group == own_group(),
        p if p < 0 => child.group == -p,
        p => child.pid == p,
    }
}

/// A child that has ended, taken out of the table: its pid, its end and its
/// accounting.
fn reap(pid: i32) -> Option<(i32, End, toyos_abi::syscall::ProcessStats)> {
    let mut children = CHILDREN.lock();
    let at = children.iter().position(|c| c.pid == pid)?;
    let end = children[at].process.try_wait().ok()?;
    let child = children.remove(at);
    UNWAITED.fetch_sub(1, Ordering::Relaxed);
    // An ended child's accounting stays on its object, which answers.
    let stats = child.process.stats().expect("waitpid: an ended child's accounting");
    Some((pid, end, stats))
}

/// The first child `pid` names that has ended, waiting for one where
/// `block`; `Ok(None)` for none ended without it.
fn wait_child(pid: i32, block: bool) -> Result<Option<(i32, End, toyos_abi::syscall::ProcessStats)>, i32> {
    loop {
        // Copies of the handles, watched with the table given up: a reap on
        // another thread closes the table's own.
        let watched: Vec<(i32, RawHandle)> = {
            let children = CHILDREN.lock();
            let named: Vec<&Child> = children.iter().filter(|c| names(pid, c)).collect();
            if named.is_empty() {
                return Err(ECHILD);
            }
            if let Some(ended) = named.iter().find(|c| c.process.try_wait().is_ok()) {
                let pid = ended.pid;
                drop(children);
                if let Some(reaped) = reap(pid) {
                    return Ok(Some(reaped));
                }
                continue;
            }
            if !block {
                return Ok(None);
            }
            let mut copies = Vec::with_capacity(named.len());
            for child in named {
                match syscall::dup(toyos::AsHandle::as_handle(&child.process)) {
                    Ok(copy) => copies.push((child.pid, copy)),
                    Err(e) => {
                        copies.into_iter().for_each(|(_, h)| syscall::close(h));
                        return Err(crate::errcode::of(e));
                    }
                }
            }
            copies
        };
        let poller = toyos::poller::Poller::new(watched.len() as u32);
        for (i, (_, handle)) in watched.iter().enumerate() {
            poller.watch_raw(*handle, toyos::poller::READABLE, i as u64);
        }
        let mut first = None;
        poller.wait(1, u64::MAX, |token| {
            first.get_or_insert(token as usize);
        });
        drop(poller);
        watched.iter().for_each(|(_, h)| syscall::close(*h));
        if let Some(reaped) = first.and_then(|i| reap(watched[i].0)) {
            return Ok(Some(reaped));
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32 {
    unsafe { wait4(pid, status, options, ptr::null_mut()) }
}

#[no_mangle]
pub unsafe extern "C" fn wait(status: *mut i32) -> i32 {
    unsafe { wait4(-1, status, 0, ptr::null_mut()) }
}

/// `<sys/resource.h>`'s `struct rusage`, up to what is written: ToyOS counts
/// a process's CPU time whole, so all of it is user time.
#[repr(C)]
pub struct Rusage {
    utime: crate::time::Timeval,
    stime: crate::time::Timeval,
    maxrss: i64,
    rest: [i64; 13],
}

/// No child stops or continues, so `WUNTRACED` and `WCONTINUED` ask nothing
/// that ever happens.
#[no_mangle]
pub unsafe extern "C" fn wait4(pid: i32, status: *mut i32, options: i32, usage: *mut Rusage) -> i32 {
    if options & !(WNOHANG | WUNTRACED | WCONTINUED) != 0 {
        crate::errno::set(EINVAL);
        return -1;
    }
    match wait_child(pid, options & WNOHANG == 0) {
        Ok(Some((pid, end, stats))) => {
            if let Some(status) = unsafe { status.as_mut() } {
                *status = spawnreq::wait_status(end);
            }
            if let Some(usage) = unsafe { usage.as_mut() } {
                let us = stats.cpu_ns / 1000;
                *usage = Rusage {
                    utime: crate::time::Timeval { tv_sec: (us / 1_000_000) as i64, tv_usec: (us % 1_000_000) as i64 },
                    stime: crate::time::Timeval { tv_sec: 0, tv_usec: 0 },
                    maxrss: (stats.peak_memory / 1024) as i64,
                    rest: [0; 13],
                };
            }
            pid
        }
        Ok(None) => 0,
        Err(errno) => {
            crate::errno::set(errno);
            -1
        }
    }
}

/// `SIGKILL` for a child or a group of them, and 0 to ask whether one is
/// there; no other signal reaches a process yet, so it is `EINVAL`. A process
/// holds no right to kill itself, so `SIGKILL` for its own pid is `EPERM`, and
/// a kill of its own group reaches its children in it and not itself.
#[no_mangle]
pub unsafe extern "C" fn kill(pid: i32, sig: i32) -> i32 {
    let me = own_group();
    let refused = match (sig, pid == me) {
        (0 | SIGKILL, false) => {
            let mut any = false;
            for child in CHILDREN.lock().iter().filter(|c| names(pid, c)) {
                any = true;
                if sig == SIGKILL {
                    // `Ok` for one already ended, which is still unwaited.
                    let _ = child.process.kill();
                }
            }
            (!any).then_some(ESRCH)
        }
        (0, true) => None,
        (SIGKILL, true) => Some(EPERM),
        _ => Some(EINVAL),
    };
    match refused {
        None => 0,
        Some(errno) => {
            crate::errno::set(errno);
            -1
        }
    }
}

/// `command` run by the shell, waited for: its status, or -1. A null
/// `command` asks whether there is a shell, and there is.
#[no_mangle]
pub unsafe extern "C" fn system(command: *const u8) -> i32 {
    if command.is_null() {
        return 1;
    }
    let pid = match shell(command, &[]) {
        Ok(pid) => pid,
        Err(errno) => {
            crate::errno::set(errno);
            return -1;
        }
    };
    let mut status = 0;
    match unsafe { waitpid(pid, &mut status, 0) } {
        -1 => -1,
        _ => status,
    }
}

/// Start `/system/bin/shell -c command`, its descriptors as `actions` leave them.
fn shell(command: *const u8, actions: &[Action<Box<[u8]>>]) -> Result<i32, i32> {
    let mut argv = Vec::new();
    for arg in [SHELL.as_bytes(), b"-c", crate::posix_io::c_str_to_bytes(command)] {
        argv.extend_from_slice(arg);
        argv.push(0);
    }
    spawn_child(SHELL, &argv, &crate::env::blob(), actions, None)
}

/// Each stream `popen` made, by its address, and the child behind it.
static POPENED: Lock<Vec<(usize, i32)>> = Lock::new(Vec::new());

#[no_mangle]
pub unsafe extern "C" fn popen(command: *const u8, mode: *const u8) -> *mut crate::stdio::FILE {
    let reading = match crate::posix_io::c_str_to_bytes(mode) {
        b"r" => true,
        b"w" => false,
        _ => {
            crate::errno::set(EINVAL);
            return ptr::null_mut();
        }
    };
    let ends = match syscall::pipe() {
        Ok(ends) => ends,
        Err(e) => {
            crate::errno::set(crate::errcode::of(e));
            return ptr::null_mut();
        }
    };
    let (theirs, ours, child) = match reading {
        true => (ends.write, ends.read, 1),
        false => (ends.read, ends.write, 0),
    };
    // Both ends as descriptors, this process's own closed on exec.
    let (theirs, ours) = match (crate::posix_io::install(theirs, true), crate::posix_io::install(ours, true)) {
        (Ok(theirs), Ok(ours)) => (theirs, ours),
        (theirs, ours) => {
            for fd in [theirs, ours].into_iter().flatten() {
                let _ = toyos::fd::close(fd);
            }
            crate::errno::set(crate::errcode::EMFILE);
            return ptr::null_mut();
        }
    };
    let started = shell(command, &[Action::Dup2 { fd: theirs as u32, newfd: child }]);
    let _ = toyos::fd::close(theirs);
    let pid = match started {
        Ok(pid) => pid,
        Err(errno) => {
            let _ = toyos::fd::close(ours);
            crate::errno::set(errno);
            return ptr::null_mut();
        }
    };
    let stream = unsafe { crate::stdio::fdopen(ours, mode) };
    if stream.is_null() {
        // No stream to read the child by: its end goes, and the child is waited for.
        let _ = toyos::fd::close(ours);
        unsafe { waitpid(pid, ptr::null_mut(), 0) };
        crate::errno::set(crate::errcode::ENOMEM);
        return ptr::null_mut();
    }
    POPENED.lock().push((stream as usize, pid));
    stream
}

#[no_mangle]
pub unsafe extern "C" fn pclose(stream: *mut crate::stdio::FILE) -> i32 {
    let pid = {
        let mut popened = POPENED.lock();
        let Some(at) = popened.iter().position(|&(s, _)| s == stream as usize) else {
            crate::errno::set(ECHILD);
            return -1;
        };
        popened.remove(at).1
    };
    unsafe { crate::stdio::fclose(stream) };
    let mut status = 0;
    match unsafe { waitpid(pid, &mut status, 0) } {
        -1 => -1,
        _ => status,
    }
}
