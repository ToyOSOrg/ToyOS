//! Children: `posix_spawn` and `posix_spawnp` with their file actions and
//! attributes, the wait family, `kill`, and `system`, `popen` and `pclose`
//! through `/system/bin/shell -c`.
//!
//! **A child is the `Process` handle its spawn answered**, which this library
//! keeps under its pid until it is waited for; a pid names nothing else here,
//! and `kill` and `waitpid` reach only this process's own children. A child
//! is a helper (`toyos::spawn`), as a plain std spawn is. It starts with descriptors
//! 0–2 and exactly what its file actions name (`spawnreq`). A child's group is
//! a number this library keeps, which `kill(-pgid, …)` and `waitpid(-pgid,
//! …)` read; the kernel has none.
//!
//! **A child's end raises `SIGCHLD` once** (`crate::signal`): libc's own
//! thread watches every child that has not raised it, while a handler is
//! installed for it, and a wait that takes a child it had not seen end
//! tells it so. With `SIGCHLD` ignored an ended child is not kept, so a wait
//! for children blocks until every one it names has ended and answers
//! `ECHILD`.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::ptr;
use core::sync::atomic::{AtomicUsize, Ordering};

use toyos::fd::Kind;
use toyos::process::{End, Process};
use toyos::spawn::{Parent, Refused, Spawn, Start};
use toyos_abi::syscall;

use crate::errcode::{E2BIG, EACCES, EAGAIN, EBADF, ECHILD, EINTR, EINVAL, ENOENT, EPERM, ESRCH};
use crate::pthread::Lock;
use crate::spawnreq::{self, Action, Turn};

const SIGKILL: i32 = 9;
const WNOHANG: i32 = 1;
const WUNTRACED: i32 = 2;
const WCONTINUED: i32 = 8;

const POSIX_SPAWN_RESETIDS: i16 = 0x01;
const POSIX_SPAWN_SETPGROUP: i16 = 0x02;
const POSIX_SPAWN_SETSIGDEF: i16 = 0x04;
const POSIX_SPAWN_SETSIGMASK: i16 = 0x08;
/// The flags a spawn honours. A child starts with every signal at its default
/// and none blocked, which is what `SETSIGDEF` asks and an empty `SETSIGMASK`
/// asks; one naming a signal is refused at the spawn. A process has no ids to
/// reset.
const FLAGS: i16 = POSIX_SPAWN_RESETIDS | POSIX_SPAWN_SETPGROUP | POSIX_SPAWN_SETSIGDEF | POSIX_SPAWN_SETSIGMASK;

/// The most children unwaited at once: libc's own thread watches them all on
/// one poller, beside its own control event and the process's notice.
pub(crate) const MAX_CHILDREN: usize = toyos::poller::Poller::MAX_HANDLES as usize - 2;

/// What the shell `system` and `popen` run a command with.
const SHELL: &str = "/system/bin/shell";

struct Child {
    pid: i32,
    group: i32,
    process: Process,
    /// Whether its end has raised `SIGCHLD`.
    raised: bool,
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
        // The child starts blocking nothing, so a mask that names a signal
        // would be a mask it does not have.
        Some(attr) if attr.flags & POSIX_SPAWN_SETSIGMASK != 0 && attr.mask != 0 => return crate::errcode::ENOSYS,
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
    if crate::signal::children_ignored() {
        forget_ended();
    }
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
            let Some((copy, kind)) = toyos::fd::copy(fd as i32).map_err(errno_of)? else { return Ok(None) };
            let Ok(inheritable) = toyos::fd::inheritable(fd as i32) else { return Ok(None) };
            let handle = toyos::AsHandle::as_handle(&copy).raw();
            held.push(copy);
            Ok(Some(((handle, kind), !inheritable)))
        },
        actions,
        |path, oflag, mode| crate::posix_io::open_handle(path, oflag, mode).map(|h| (h, Kind::Handle)),
        // SAFETY: a file the plan opened and names nowhere else.
        |(handle, _)| unsafe { syscall::close(handle) },
    )?;
    // SAFETY: each a file the plan opened or a copy `held` owns, both closed
    // only once the spawn has returned.
    let fds: Vec<(u32, toyos::BorrowedHandle<'_>, Kind)> =
        plan.fds.iter().map(|&(fd, (handle, kind))| (fd, unsafe { toyos::BorrowedHandle::borrow_raw(handle) }, kind)).collect();
    let started = start(path, argv, env, &fds);
    for (handle, _) in plan.opened {
        // SAFETY: a file the plan opened, closed once.
        unsafe { syscall::close(handle) };
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
    CHILDREN.lock().push(Child { pid, group, process, raised: false });
    crate::signal::children_changed(false);
    Ok(pid)
}

/// A helper, from this process's own working directory: C's children are
/// never launches.
fn start(path: &str, argv: &[u8], env: &[u8], fds: &[(u32, toyos::BorrowedHandle<'_>, Kind)]) -> Result<Process, i32> {
    let mut cwd = alloc::vec![0u8; syscall::MAX_PATH];
    let n = syscall::getcwd(&mut cwd);
    let cwd = core::str::from_utf8(&cwd[..n]).map_err(|_| EINVAL)?;
    let request = Spawn {
        program: path,
        image: None,
        image_held: false,
        launcher: None,
        pager: None,
        argv,
        env,
        cwd,
        fds,
        endow: Vec::new(),
        provided: Vec::new(),
        parent: Parent::Caller,
        independent: false,
    };
    let Start::Helper(helper) = request.start().map_err(errno_of)? else {
        unreachable!("a spawn that is not independent is a helper or refused")
    };
    // A program on a file server is read here into an image the kernel pages
    // the child from: the kernel opens only what it serves itself.
    let image = match toyos::files::is_served(path.as_bytes()) {
        true => Some(toyos::files::read_image(path.as_bytes()).map_err(crate::errcode::of_image)?),
        false => None,
    };
    helper.spawn(image.as_ref()).map_err(errno_of)
}

fn errno_of(refused: Refused) -> i32 {
    match refused {
        Refused::Permission(_) | Refused::NotStarted => EACCES,
        Refused::Invalid(_) => E2BIG,
        Refused::Stream(_) => EBADF,
        // A holder past what netstack has places for is a descriptor too many.
        Refused::Export(toyos::net::NetError::ResourceExhausted) => crate::errcode::EMFILE,
        Refused::Export(_) => crate::errcode::EIO,
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
    drop(children);
    UNWAITED.fetch_sub(1, Ordering::Relaxed);
    crate::signal::children_changed(!child.raised);
    // An ended child's accounting stays on its object, which answers.
    let stats = child.process.stats().expect("waitpid: an ended child's accounting");
    Some((pid, end, stats))
}

/// Every child that has ended, out of the table and kept for no wait, as
/// `SIGCHLD` ignored has it.
pub(crate) fn forget_ended() {
    let mut children = CHILDREN.lock();
    let ended: Vec<bool> = children.iter().map(|c| c.process.try_wait().is_ok()).collect();
    forget(&mut children, &ended);
}

/// Take the children `ended` read as ended out of `children`, kept for no
/// wait.
fn forget(children: &mut Vec<Child>, ended: &[bool]) {
    let mut at = 0;
    children.retain(|_| {
        at += 1;
        !ended[at - 1]
    });
    UNWAITED.fetch_sub(ended.iter().filter(|&&e| e).count(), Ordering::Relaxed);
}

/// Each child whose end has not raised `SIGCHLD`, by pid.
pub(crate) fn unraised() -> Vec<i32> {
    CHILDREN.lock().iter().filter(|c| !c.raised).map(|c| c.pid).collect()
}

/// A copy of child `pid`'s handle to watch its end on, the caller's to close:
/// a wait on another thread closes the table's own, whose number another
/// handle may then take. `None` for a child gone, or a table with no room.
pub(crate) fn copy_of(pid: i32) -> Option<toyos::OwnedHandle> {
    let children = CHILDREN.lock();
    let child = children.iter().find(|c| c.pid == pid)?;
    toyos::AsHandle::as_handle(&child.process).try_clone_to_owned().ok()
}

/// Whether `pid` has ended and not yet raised `SIGCHLD`, which it now has.
pub(crate) fn raise(pid: i32) -> bool {
    let mut children = CHILDREN.lock();
    let Some(child) = children.iter_mut().find(|c| c.pid == pid && !c.raised) else { return false };
    child.raised = child.process.try_wait().is_ok();
    child.raised
}

/// Every child that has ended raises no `SIGCHLD`: a handler installed after
/// an end is not told of it.
pub(crate) fn mark_ended_raised() {
    for child in CHILDREN.lock().iter_mut() {
        child.raised |= child.process.try_wait().is_ok();
    }
}

/// The first child `pid` names that has ended, waiting for one where
/// `block`; `Ok(None)` for none ended without it. A handler that interrupts
/// the wait restarts it if it was installed with `SA_RESTART` and answers
/// `EINTR` if not; a child started while it waits is one it waits for.
fn wait_child(pid: i32, block: bool) -> Result<Option<(i32, End, toyos_abi::syscall::ProcessStats)>, i32> {
    let mut call = crate::signal::Call::enter(None, true, false).expect("a wait for children is always a call");
    loop {
        if let Some(delivery) = call.take() {
            if !call.interrupted(delivery) {
                return Err(EINTR);
            }
            continue;
        }
        // Before the table is read: a child started after it moves the turn
        // this records, and the wake event says so.
        call.children_seen();
        let ignored = crate::signal::children_ignored();
        // Copies of the handles, watched with the table given up: a reap on
        // another thread closes the table's own.
        let watched: Vec<(i32, toyos::OwnedHandle)> = {
            let mut children = CHILDREN.lock();
            // Each child's end is read once, and that read alone decides: a
            // child that ends after it is the next turn's.
            let ended: Vec<bool> = children.iter().map(|c| c.process.try_wait().is_ok()).collect();
            let named: Vec<usize> = (0..children.len()).filter(|&i| names(pid, &children[i])).collect();
            let named_ended: Vec<bool> = named.iter().map(|&i| ended[i]).collect();
            let turn = spawnreq::turn(&named_ended, ignored);
            if ignored {
                forget(&mut children, &ended);
            }
            let running: Vec<&Child> = match turn {
                Turn::Gone => return Err(ECHILD),
                Turn::Take(at) => {
                    let pid = children[named[at]].pid;
                    drop(children);
                    if let Some(reaped) = reap(pid) {
                        return Ok(Some(reaped));
                    }
                    continue;
                }
                Turn::Watch if !block => return Ok(None),
                Turn::Watch => children.iter().filter(|c| names(pid, c)).collect(),
            };
            let mut copies = Vec::with_capacity(running.len());
            for child in running {
                match toyos::AsHandle::as_handle(&child.process).try_clone_to_owned() {
                    Ok(copy) => copies.push((child.pid, copy)),
                    Err(e) => return Err(crate::errcode::of(e)),
                }
            }
            copies
        };
        let poller = toyos::poller::Poller::new(watched.len() as u32 + 1);
        for (i, (_, handle)) in watched.iter().enumerate() {
            poller.watch(handle, toyos::poller::READABLE, i as u64);
        }
        poller.watch(&call.wake(), toyos::poller::READABLE, u64::MAX);
        // Whatever woke it, the next turn looks: an ended child is reaped
        // there, or forgotten under an ignored `SIGCHLD`.
        poller.wait(1, u64::MAX, |_| {});
        drop(poller);
        drop(watched);
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

/// `SIGKILL` for a child or a group of them, `SIGINT`, `SIGHUP` and
/// `SIGTERM` as a quit with that reason, which asks the child and everything
/// below it as a POSIX `kill` does not, and 0 to ask whether one is there; no
/// other signal reaches another process, so it is `EINVAL`. A process holds
/// no right to kill itself, so `SIGKILL` for its own pid is `EPERM`, and one
/// of the three is raised in it, as it is by a kill of its own group, which
/// reaches its children in it too.
#[no_mangle]
pub unsafe extern "C" fn kill(pid: i32, sig: i32) -> i32 {
    let me = own_group();
    let quit = quit_of(sig);
    let refused = match (sig, pid == me) {
        (_, false) if sig == 0 || sig == SIGKILL || quit.is_some() => {
            let mut any = false;
            for child in CHILDREN.lock().iter().filter(|c| names(pid, c)) {
                any = true;
                // `Ok` for one already ended, which is still unwaited, and
                // `MANAGE` is on every child's handle libc holds.
                let asked = match quit {
                    Some(reason) => child.process.quit(reason),
                    None if sig == SIGKILL => child.process.kill(),
                    None => Ok(()),
                };
                asked.expect("kill: a child's handle refused what its spawn gave it");
            }
            let own_group = pid == 0 || pid == -me;
            match quit.is_some() && own_group {
                true => crate::signal::raise(sig).err(),
                false => (!any).then_some(ESRCH),
            }
        }
        (0, true) => None,
        (SIGKILL, true) => Some(EPERM),
        // A quit of this process is its own signal, raised here.
        (_, true) if quit.is_some() => crate::signal::raise(sig).err(),
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

/// The reason a quit asks for `sig`, for one of the three it raises.
fn quit_of(sig: i32) -> Option<toyos::quit::Reason> {
    use crate::sigreq::{SIGHUP, SIGINT, SIGTERM};
    use toyos::quit::Reason;
    match sig {
        SIGINT => Some(Reason::Interrupt),
        SIGHUP => Some(Reason::HangUp),
        SIGTERM => Some(Reason::Terminate),
        _ => None,
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
    let (read, write) = match toyos::pipe_pair() {
        Ok(ends) => ends,
        Err(e) => {
            crate::errno::set(crate::errcode::of(e));
            return ptr::null_mut();
        }
    };
    let (theirs, ours, child) = match reading {
        true => (write, read, 1),
        false => (read, write, 0),
    };
    // Both ends as descriptors, this process's own closed on exec.
    let (theirs, ours) = match (crate::posix_io::install(theirs.into(), true), crate::posix_io::install(ours.into(), true)) {
        (Ok(theirs), Ok(ours)) => (theirs, ours),
        (theirs, ours) => {
            for fd in [theirs, ours].into_iter().flatten() {
                // SAFETY: an end installed just above, named nowhere else.
                let _ = unsafe { toyos::fd::close(fd) };
            }
            crate::errno::set(crate::errcode::EMFILE);
            return ptr::null_mut();
        }
    };
    let started = shell(command, &[Action::Dup2 { fd: theirs as u32, newfd: child }]);
    // SAFETY: the child's end, this function's own, which the spawn copied.
    let _ = unsafe { toyos::fd::close(theirs) };
    let pid = match started {
        Ok(pid) => pid,
        Err(errno) => {
            // SAFETY: this function's own end, which no stream took.
            let _ = unsafe { toyos::fd::close(ours) };
            crate::errno::set(errno);
            return ptr::null_mut();
        }
    };
    let stream = unsafe { crate::stdio::fdopen(ours, mode) };
    if stream.is_null() {
        // No stream to read the child by: its end goes, and the child is waited for.
        // SAFETY: this function's own end, which no stream took.
        let _ = unsafe { toyos::fd::close(ours) };
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
