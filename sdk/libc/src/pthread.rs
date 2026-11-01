//! POSIX threads on the kernel's thread and futex syscalls.
//!
//! **A `pthread_t` is the address of the thread's [`Thread`] block**, which
//! `pthread_create` allocates and the new thread records in [`SELF`]; a
//! thread this library did not start (the main thread, or one Rust's std
//! spawned) is named by the address of its own [`MARKER`] with [`FOREIGN`]
//! set, which no block's address has, and cannot be joined or detached.
//!
//! **No `pthread_t` exists before its thread's tid is recorded**: the new
//! thread waits for its creator to publish the tid before it runs anything,
//! so every holder of a handle can join it.
//!
//! A thread that ends detached hands its block to [`REAPED`], and the next
//! `pthread_create` joins it and frees its stack, which no thread can free
//! while standing on it.

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::cell::Cell;
use core::ptr;
use core::sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, Ordering};

use toyos_abi::syscall;

use crate::errcode::{EAGAIN, EBUSY, EDEADLK, EINVAL, EPERM, ESRCH, ETIMEDOUT};

type PthreadT = u64;
type StartRoutine = unsafe extern "C" fn(*mut u8) -> *mut u8;

const STACK_DEFAULT: usize = 1024 * 1024;
const STACK_MIN: usize = 16 * 1024;
const STACK_ALIGN: usize = 16;

// `pthread_attr_t`: the stack size, with the detach state in its low bit.
const ATTR_DETACHED: u64 = 1;

/// One thread `pthread_create` started.
struct Thread {
    tid: AtomicU64,
    /// Nonzero once `tid` is written; the thread waits for it before it runs.
    published: AtomicU32,
    state: AtomicU32,
    /// The stack's reservation: [`GUARD`] reserved, then the stack committed.
    reservation: *mut u8,
    reserved: usize,
    start: StartRoutine,
    arg: *mut u8,
    result: AtomicPtr<u8>,
    /// Whether a signal is routed by its mask: every thread but libc's own.
    counted: bool,
}

/// The bit a `pthread_t` of a thread this library did not start carries.
const FOREIGN: PthreadT = 1;

const JOINABLE: u32 = 0;
const DETACHED: u32 = 1;
const EXITED: u32 = 2;

#[thread_local]
static SELF: Cell<*mut Thread> = Cell::new(ptr::null_mut());

#[thread_local]
static MARKER: u8 = 0;

/// Blocks of detached threads that have ended, for the next creator to reap.
static REAPED: Lock<Vec<usize>> = Lock::new(Vec::new());

pub(crate) use toyos::sync::Lock;

/// Reserved pages below a thread's stack, so that running off it faults:
/// one page, which C's frames reach through only by skipping it, as a guard
/// page on every POSIX system can be skipped.
const GUARD: usize = syscall::PAGE_SIZE;

/// A stack of `size` bytes in whole pages above a [`GUARD`], committed before
/// the thread starts: the reservation, its length, and the stack's base.
fn reserve_stack(size: usize) -> Option<(*mut u8, usize, *mut u8)> {
    let pages = size.checked_next_multiple_of(syscall::PAGE_SIZE)?;
    let reserved = pages.checked_add(GUARD)?;
    // SAFETY: placed anywhere, so it replaces nothing.
    let reservation = unsafe { syscall::vm_reserve(reserved, syscall::VmProt::None, syscall::VmAt::ANYWHERE) }.ok()?;
    let base = reservation.wrapping_add(GUARD);
    // SAFETY: the stack's pages, inside the reservation just made.
    if unsafe { syscall::vm_protect(base, pages, syscall::VmProt::ReadWrite) }.is_err() {
        // SAFETY: the reservation just made, which nothing reaches.
        let _ = unsafe { syscall::vm_release(reservation, reserved) };
        return None;
    }
    Some((reservation, reserved, base))
}

/// Wait for `thread` to be gone, free its stack and block, and answer what it
/// returned.
unsafe fn reap(thread: *mut Thread) -> *mut u8 {
    let t = unsafe { &*thread };
    syscall::thread_join(t.tid.load(Ordering::Relaxed));
    let result = t.result.load(Ordering::Acquire);
    unsafe {
        let _ = syscall::vm_release(t.reservation, t.reserved);
        drop(Box::from_raw(thread));
    }
    result
}

unsafe extern "C" fn thread_entry(arg: u64) {
    let thread = arg as *mut Thread;
    let t = unsafe { &*thread };
    while t.published.load(Ordering::Acquire) == 0 {
        // SAFETY: `published` is a live, aligned u32.
        unsafe { syscall::futex_wait(t.published.as_ptr(), 0, None) };
    }
    SELF.set(thread);
    let result = unsafe { (t.start)(t.arg) };
    unsafe { exit_thread(result) }
}

/// Run what a thread owes on its way out, record `result`, and end it.
unsafe fn exit_thread(result: *mut u8) -> ! {
    unsafe {
        run_thread_dtors();
        run_key_destructors();
    }
    let thread = SELF.get();
    if !thread.is_null() {
        let t = unsafe { &*thread };
        if t.counted {
            crate::signal::thread_ended(toyos_abi::current_tid().raw());
        }
        t.result.store(result, Ordering::Release);
        if t.state.swap(EXITED, Ordering::AcqRel) == DETACHED {
            REAPED.lock().push(thread as usize);
        }
    }
    syscall::thread_exit(0)
}

#[no_mangle]
pub unsafe extern "C" fn pthread_create(
    thread: *mut PthreadT,
    attr: *const u64,
    start_routine: StartRoutine,
    arg: *mut u8,
) -> i32 {
    match unsafe { create(attr, start_routine, arg, true) } {
        Ok(block) => {
            if !thread.is_null() {
                unsafe { *thread = block as PthreadT };
            }
            0
        }
        Err(errno) => errno,
    }
}

/// A detached thread of libc's own running `start`, which no signal is
/// routed by.
pub(crate) fn spawn_own(start: StartRoutine) {
    let attr = attr_default() | ATTR_DETACHED;
    // SAFETY: `attr` is a `pthread_attr_t`, and `start` takes no argument.
    let started = unsafe { create(&attr, start, ptr::null_mut(), false) };
    started.expect("libc: no thread of its own");
}

/// Start a thread, `counted` among those a signal is routed by: its block,
/// or an `errno`.
///
/// # Safety
/// `attr` is null or a `pthread_attr_t`, and `start_routine` takes `arg`.
unsafe fn create(attr: *const u64, start_routine: StartRoutine, arg: *mut u8, counted: bool) -> Result<*mut Thread, i32> {
    let reaped = core::mem::take(&mut *REAPED.lock());
    for block in reaped {
        unsafe { reap(block as *mut Thread) };
    }

    let attr = if attr.is_null() { attr_default() } else { unsafe { *attr } };
    let stack_size = (attr & !ATTR_DETACHED) as usize;
    let Some((reservation, reserved, stack)) = reserve_stack(stack_size) else {
        return Err(EAGAIN);
    };
    let state = if attr & ATTR_DETACHED != 0 { DETACHED } else { JOINABLE };
    let block = Box::into_raw(Box::new(Thread {
        tid: AtomicU64::new(0),
        published: AtomicU32::new(0),
        state: AtomicU32::new(state),
        reservation,
        reserved,
        start: start_routine,
        arg,
        result: AtomicPtr::new(ptr::null_mut()),
        counted,
    }));
    let top = (reservation as usize + reserved) & !(STACK_ALIGN - 1);
    // SAFETY: the entry is a function of this library, and the stack is the
    // committed top of a fresh reservation.
    let tid = unsafe { syscall::thread_spawn(thread_entry as *const () as u64, top as u64, block as u64, stack as u64) };
    if syscall::SyscallError::from_u64(tid).is_some() {
        unsafe {
            let _ = syscall::vm_release(reservation, reserved);
            drop(Box::from_raw(block));
        }
        return Err(EAGAIN);
    }
    // Before it runs: it starts with its creator's mask, as POSIX has it.
    if counted {
        crate::signal::thread_started(tid as u32, crate::signal::mask());
    }
    // Taken before the store: once `published` is set the thread may run, end
    // detached and be reaped, so nothing after it reads the block.
    let published = unsafe { (*block).published.as_ptr() };
    unsafe {
        (*block).tid.store(tid, Ordering::Relaxed);
        (*block).published.store(1, Ordering::Release);
    }
    // At a freed block's address this is a spurious wake, which every futex
    // waiter here re-checks its word against.
    unsafe { syscall::futex_wake(published, u32::MAX) };
    Ok(block)
}

#[no_mangle]
pub unsafe extern "C" fn pthread_join(thread: PthreadT, retval: *mut *mut u8) -> i32 {
    if thread == pthread_self() {
        return EDEADLK;
    }
    if thread == 0 || thread & FOREIGN != 0 {
        return ESRCH;
    }
    let block = thread as *mut Thread;
    if unsafe { &*block }.state.load(Ordering::Acquire) == DETACHED {
        return EINVAL;
    }
    let result = unsafe { reap(block) };
    if !retval.is_null() {
        unsafe { *retval = result };
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn pthread_detach(thread: PthreadT) -> i32 {
    if thread == 0 || thread & FOREIGN != 0 {
        return ESRCH;
    }
    let block = thread as *mut Thread;
    match unsafe { &*block }.state.compare_exchange(JOINABLE, DETACHED, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => 0,
        Err(EXITED) => {
            unsafe { reap(block) };
            0
        }
        Err(_) => EINVAL,
    }
}

#[no_mangle]
pub unsafe extern "C" fn pthread_exit(retval: *mut u8) -> ! {
    unsafe { exit_thread(retval) }
}

#[no_mangle]
pub extern "C" fn pthread_self() -> PthreadT {
    let thread = SELF.get();
    if thread.is_null() { ptr::addr_of!(MARKER) as PthreadT | FOREIGN } else { thread as PthreadT }
}

#[no_mangle]
pub extern "C" fn pthread_equal(t1: PthreadT, t2: PthreadT) -> i32 {
    (t1 == t2) as i32
}

/// The calling thread's signal mask, changed as `how` says when `set` is not
/// null, answered in `old` when that is not. A pending signal the change
/// unblocks is delivered before this returns. `ENOSYS`, the mask unchanged,
/// for blocking a quit's signal in a process that holds no notice to listen to.
#[no_mangle]
pub unsafe extern "C" fn pthread_sigmask(how: i32, set: *const u64, old: *mut u64) -> i32 {
    let mask = crate::signal::mask();
    let next = match unsafe { set.as_ref() } {
        None => None,
        Some(&set) => match crate::sigmask::changed(mask, how, set) {
            Some(next) => Some(next),
            None => return EINVAL,
        },
    };
    if !old.is_null() {
        unsafe { *old = mask };
    }
    match next.map(crate::signal::set_mask) {
        Some(Err(errno)) => errno,
        Some(Ok(())) | None => 0,
    }
}

/// Linux's, for the calling thread: `pid` 0, the one thread this library can
/// name. Any other is `ESRCH`.
///
/// # Safety
/// `mask` points at `cpusetsize` readable bytes.
#[no_mangle]
pub unsafe extern "C" fn sched_setaffinity(pid: i32, cpusetsize: usize, mask: *const u8) -> i32 {
    if pid != 0 {
        return crate::posix_io::refuse(ESRCH);
    }
    let set = unsafe { core::slice::from_raw_parts(mask, cpusetsize) };
    match crate::affinity::requested(set, toyos_abi::syscall::cpu_count() as usize) {
        Ok(words) => match toyos_abi::syscall::thread_set_affinity(u64::from(toyos_abi::current_tid().0), &words) {
            Ok(()) => 0,
            Err(e) => crate::posix_io::set_errno(e),
        },
        Err(errno) => crate::posix_io::refuse(errno),
    }
}

/// # Safety
/// `mask` points at `cpusetsize` writable bytes.
#[no_mangle]
pub unsafe extern "C" fn sched_getaffinity(pid: i32, cpusetsize: usize, mask: *mut u8) -> i32 {
    if pid != 0 {
        return crate::posix_io::refuse(ESRCH);
    }
    let words = match toyos_abi::syscall::thread_affinity(u64::from(toyos_abi::current_tid().0)) {
        Ok(words) => words,
        Err(e) => return crate::posix_io::set_errno(e),
    };
    let set = unsafe { core::slice::from_raw_parts_mut(mask, cpusetsize) };
    match crate::affinity::fill(set, &words, toyos_abi::syscall::cpu_count() as usize) {
        Ok(()) => 0,
        Err(errno) => crate::posix_io::refuse(errno),
    }
}

#[no_mangle]
pub extern "C" fn sched_getcpu() -> i32 {
    toyos_abi::syscall::current_cpu() as i32
}

#[no_mangle]
pub extern "C" fn sched_yield() -> i32 {
    // No syscall gives the processor up; this is what std's `yield_now` does.
    core::hint::spin_loop();
    0
}

// Mutex

const MUTEX_NORMAL: u32 = 0;
const MUTEX_RECURSIVE: u32 = 1;
const MUTEX_ERRORCHECK: u32 = 2;

#[repr(C)]
pub struct PthreadMutexT {
    state: AtomicU32,
    kind: u32,
    owner: AtomicU64,
    count: AtomicU64,
}

#[no_mangle]
pub unsafe extern "C" fn pthread_mutex_init(mutex: *mut PthreadMutexT, attr: *const u64) -> i32 {
    let kind = if attr.is_null() { MUTEX_NORMAL } else { unsafe { *attr as u32 } };
    unsafe {
        mutex.write(PthreadMutexT { state: AtomicU32::new(0), kind, owner: AtomicU64::new(0), count: AtomicU64::new(0) });
    }
    0
}

#[no_mangle]
pub unsafe extern "C" fn pthread_mutex_lock(mutex: *mut PthreadMutexT) -> i32 {
    let m = unsafe { &*mutex };
    let me = pthread_self();
    if m.kind != MUTEX_NORMAL && m.owner.load(Ordering::Relaxed) == me {
        if m.kind == MUTEX_ERRORCHECK {
            return EDEADLK;
        }
        m.count.fetch_add(1, Ordering::Relaxed);
        return 0;
    }
    toyos::sync::lock_word(&m.state);
    m.owner.store(me, Ordering::Relaxed);
    m.count.store(1, Ordering::Relaxed);
    0
}

#[no_mangle]
pub unsafe extern "C" fn pthread_mutex_trylock(mutex: *mut PthreadMutexT) -> i32 {
    let m = unsafe { &*mutex };
    let me = pthread_self();
    if m.kind == MUTEX_RECURSIVE && m.owner.load(Ordering::Relaxed) == me {
        m.count.fetch_add(1, Ordering::Relaxed);
        return 0;
    }
    if m.state.compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed).is_err() {
        return EBUSY;
    }
    m.owner.store(me, Ordering::Relaxed);
    m.count.store(1, Ordering::Relaxed);
    0
}

#[no_mangle]
pub unsafe extern "C" fn pthread_mutex_unlock(mutex: *mut PthreadMutexT) -> i32 {
    let m = unsafe { &*mutex };
    if m.kind != MUTEX_NORMAL {
        if m.owner.load(Ordering::Relaxed) != pthread_self() {
            return EPERM;
        }
        if m.count.fetch_sub(1, Ordering::Relaxed) > 1 {
            return 0;
        }
    }
    m.owner.store(0, Ordering::Relaxed);
    toyos::sync::unlock_word(&m.state);
    0
}

#[no_mangle]
pub unsafe extern "C" fn pthread_mutex_destroy(_mutex: *mut PthreadMutexT) -> i32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn pthread_mutexattr_init(attr: *mut u64) -> i32 {
    unsafe { *attr = MUTEX_NORMAL as u64 };
    0
}

#[no_mangle]
pub unsafe extern "C" fn pthread_mutexattr_destroy(_attr: *mut u64) -> i32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn pthread_mutexattr_settype(attr: *mut u64, kind: i32) -> i32 {
    match kind as u32 {
        MUTEX_NORMAL | MUTEX_RECURSIVE | MUTEX_ERRORCHECK => {
            unsafe { *attr = kind as u64 };
            0
        }
        _ => EINVAL,
    }
}

#[no_mangle]
pub unsafe extern "C" fn pthread_mutexattr_gettype(attr: *const u64, kind: *mut i32) -> i32 {
    unsafe { *kind = *attr as i32 };
    0
}

// Condition variable: a sequence number a waiter sleeps on until it moves.

#[repr(C)]
pub struct PthreadCondT {
    seq: AtomicU32,
}

#[no_mangle]
pub unsafe extern "C" fn pthread_cond_init(cond: *mut PthreadCondT, _attr: *const u64) -> i32 {
    unsafe { cond.write(PthreadCondT { seq: AtomicU32::new(0) }) };
    0
}

/// Release `mutex`, sleep until `cond` is signalled or `timeout` nanoseconds
/// pass, and take `mutex` again, as many times as it was held: 0, or
/// `ETIMEDOUT`, or `EPERM` for a recursive or error-checking mutex the caller
/// does not hold, as POSIX says.
unsafe fn cond_wait(cond: *mut PthreadCondT, mutex: *mut PthreadMutexT, timeout: Option<u64>) -> i32 {
    let m = unsafe { &*mutex };
    if m.kind != MUTEX_NORMAL && m.owner.load(Ordering::Relaxed) != pthread_self() {
        return EPERM;
    }
    let seq = unsafe { &(*cond).seq };
    let at = seq.load(Ordering::Relaxed);
    let held = m.count.swap(1, Ordering::Relaxed);
    unsafe { pthread_mutex_unlock(mutex) };
    // SAFETY: `seq` is a live, aligned u32.
    let timed_out = unsafe { syscall::futex_wait(seq.as_ptr(), at, timeout) } == 1;
    unsafe { pthread_mutex_lock(mutex) };
    m.count.store(held, Ordering::Relaxed);
    if timed_out { ETIMEDOUT } else { 0 }
}

#[no_mangle]
pub unsafe extern "C" fn pthread_cond_wait(cond: *mut PthreadCondT, mutex: *mut PthreadMutexT) -> i32 {
    unsafe { cond_wait(cond, mutex, None) }
}

#[no_mangle]
pub unsafe extern "C" fn pthread_cond_timedwait(
    cond: *mut PthreadCondT,
    mutex: *mut PthreadMutexT,
    abstime: *const crate::time::Timespec,
) -> i32 {
    let deadline = unsafe { &*abstime };
    if deadline.tv_nsec < 0 || deadline.tv_nsec >= 1_000_000_000 {
        return EINVAL;
    }
    let mut now = crate::time::Timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { crate::time::clock_gettime(crate::time::CLOCK_REALTIME, &mut now) };
    let left = (deadline.tv_sec as i128 - now.tv_sec as i128) * 1_000_000_000
        + (deadline.tv_nsec as i128 - now.tv_nsec as i128);
    if left <= 0 {
        return ETIMEDOUT;
    }
    let left = u64::try_from(left).unwrap_or(u64::MAX - 1);
    unsafe { cond_wait(cond, mutex, Some(left)) }
}

#[no_mangle]
pub unsafe extern "C" fn pthread_cond_signal(cond: *mut PthreadCondT) -> i32 {
    let seq = unsafe { &(*cond).seq };
    seq.fetch_add(1, Ordering::Release);
    // SAFETY: `seq` is a live, aligned u32.
    unsafe { syscall::futex_wake(seq.as_ptr(), 1) };
    0
}

#[no_mangle]
pub unsafe extern "C" fn pthread_cond_broadcast(cond: *mut PthreadCondT) -> i32 {
    let seq = unsafe { &(*cond).seq };
    seq.fetch_add(1, Ordering::Release);
    // SAFETY: `seq` is a live, aligned u32.
    unsafe { syscall::futex_wake(seq.as_ptr(), u32::MAX) };
    0
}

#[no_mangle]
pub unsafe extern "C" fn pthread_cond_destroy(_cond: *mut PthreadCondT) -> i32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn pthread_condattr_init(attr: *mut u64) -> i32 {
    unsafe { *attr = 0 };
    0
}

#[no_mangle]
pub unsafe extern "C" fn pthread_condattr_destroy(_attr: *mut u64) -> i32 {
    0
}

// Once

#[repr(C)]
pub struct PthreadOnceT {
    state: AtomicU32, // 0 = not called, 1 = in progress, 2 = done
}

#[no_mangle]
pub unsafe extern "C" fn pthread_once(once: *mut PthreadOnceT, init_routine: unsafe extern "C" fn()) -> i32 {
    let state = unsafe { &(*once).state };
    if state.load(Ordering::Acquire) == 2 {
        return 0;
    }
    if state.compare_exchange(0, 1, Ordering::Acquire, Ordering::Acquire).is_ok() {
        unsafe { init_routine() };
        state.store(2, Ordering::Release);
        // SAFETY: `state` is a live, aligned u32.
        unsafe { syscall::futex_wake(state.as_ptr(), u32::MAX) };
        return 0;
    }
    while state.load(Ordering::Acquire) != 2 {
        // SAFETY: `state` is a live, aligned u32.
        unsafe { syscall::futex_wait(state.as_ptr(), 1, None) };
    }
    0
}

// Keys: a process-wide table of destructors, and each thread's own values.

const KEYS_MAX: usize = 128;
const DESTRUCTOR_ITERATIONS: usize = 4;

type KeyDestructor = unsafe extern "C" fn(*mut u8);

static KEY_DESTRUCTORS: [AtomicPtr<()>; KEYS_MAX] = [const { AtomicPtr::new(ptr::null_mut()) }; KEYS_MAX];
static NEXT_KEY: AtomicU32 = AtomicU32::new(0);

#[thread_local]
static KEY_VALUES: [Cell<*mut u8>; KEYS_MAX] = [const { Cell::new(ptr::null_mut()) }; KEYS_MAX];

#[no_mangle]
pub unsafe extern "C" fn pthread_key_create(key: *mut u32, destructor: Option<KeyDestructor>) -> i32 {
    let k = NEXT_KEY.fetch_add(1, Ordering::Relaxed);
    if k as usize >= KEYS_MAX {
        return EAGAIN;
    }
    KEY_DESTRUCTORS[k as usize].store(destructor.map_or(ptr::null_mut(), |d| d as *mut ()), Ordering::Release);
    unsafe { *key = k };
    0
}

#[no_mangle]
pub unsafe extern "C" fn pthread_key_delete(key: u32) -> i32 {
    if key >= NEXT_KEY.load(Ordering::Relaxed).min(KEYS_MAX as u32) {
        return EINVAL;
    }
    KEY_DESTRUCTORS[key as usize].store(ptr::null_mut(), Ordering::Release);
    0
}

#[no_mangle]
pub unsafe extern "C" fn pthread_getspecific(key: u32) -> *mut u8 {
    KEY_VALUES.get(key as usize).map_or(ptr::null_mut(), Cell::get)
}

#[no_mangle]
pub unsafe extern "C" fn pthread_setspecific(key: u32, value: *const u8) -> i32 {
    match KEY_VALUES.get(key as usize) {
        Some(slot) => {
            slot.set(value as *mut u8);
            0
        }
        None => EINVAL,
    }
}

/// POSIX's destructor rounds: each non-null value is cleared and its key's
/// destructor called with it, until no value is set or the rounds run out.
unsafe fn run_key_destructors() {
    for _ in 0..DESTRUCTOR_ITERATIONS {
        let mut ran = false;
        for (slot, destructor) in KEY_VALUES.iter().zip(&KEY_DESTRUCTORS) {
            let value = slot.replace(ptr::null_mut());
            let destructor = destructor.load(Ordering::Acquire);
            if !value.is_null() && !destructor.is_null() {
                // SAFETY: only `pthread_key_create` stores here, and it stores a destructor.
                let destructor: KeyDestructor = unsafe { core::mem::transmute(destructor) };
                unsafe { destructor(value) };
                ran = true;
            }
        }
        if !ran {
            return;
        }
    }
}

// C++ `thread_local` destructors, which the C++ runtime registers here.

struct ThreadDtor {
    dtor: unsafe extern "C" fn(*mut u8),
    obj: *mut u8,
    next: *mut ThreadDtor,
}

#[thread_local]
static THREAD_DTORS: Cell<*mut ThreadDtor> = Cell::new(ptr::null_mut());

#[no_mangle]
pub unsafe extern "C" fn __cxa_thread_atexit_impl(
    dtor: unsafe extern "C" fn(*mut u8),
    obj: *mut u8,
    _dso_symbol: *mut u8,
) -> i32 {
    let node = Box::into_raw(Box::new(ThreadDtor { dtor, obj, next: THREAD_DTORS.get() }));
    THREAD_DTORS.set(node);
    0
}

/// Run the calling thread's `thread_local` destructors, the last registered
/// first, including any a destructor registers.
pub(crate) unsafe fn run_thread_dtors() {
    loop {
        let node = THREAD_DTORS.get();
        if node.is_null() {
            return;
        }
        let node = unsafe { Box::from_raw(node) };
        THREAD_DTORS.set(node.next);
        unsafe { (node.dtor)(node.obj) };
    }
}

// RWLock (simple: wraps mutex, no reader parallelism)

#[repr(C)]
pub struct PthreadRwlockT {
    mutex: PthreadMutexT,
}

#[no_mangle]
pub unsafe extern "C" fn pthread_rwlock_init(rwlock: *mut PthreadRwlockT, _attr: *const u8) -> i32 {
    unsafe { pthread_mutex_init(ptr::addr_of_mut!((*rwlock).mutex), ptr::null()) }
}

#[no_mangle]
pub unsafe extern "C" fn pthread_rwlock_rdlock(rwlock: *mut PthreadRwlockT) -> i32 {
    unsafe { pthread_mutex_lock(ptr::addr_of_mut!((*rwlock).mutex)) }
}

#[no_mangle]
pub unsafe extern "C" fn pthread_rwlock_wrlock(rwlock: *mut PthreadRwlockT) -> i32 {
    unsafe { pthread_mutex_lock(ptr::addr_of_mut!((*rwlock).mutex)) }
}

#[no_mangle]
pub unsafe extern "C" fn pthread_rwlock_unlock(rwlock: *mut PthreadRwlockT) -> i32 {
    unsafe { pthread_mutex_unlock(ptr::addr_of_mut!((*rwlock).mutex)) }
}

#[no_mangle]
pub unsafe extern "C" fn pthread_rwlock_destroy(rwlock: *mut PthreadRwlockT) -> i32 {
    unsafe { pthread_mutex_destroy(ptr::addr_of_mut!((*rwlock).mutex)) }
}

// Attributes

fn attr_default() -> u64 {
    STACK_DEFAULT as u64
}

#[no_mangle]
pub unsafe extern "C" fn pthread_attr_init(attr: *mut u64) -> i32 {
    unsafe { *attr = attr_default() };
    0
}

#[no_mangle]
pub unsafe extern "C" fn pthread_attr_destroy(_attr: *mut u64) -> i32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn pthread_attr_setstacksize(attr: *mut u64, size: usize) -> i32 {
    if size < STACK_MIN {
        return EINVAL;
    }
    // A size no allocation can have is refused here, not in `pthread_create`.
    let Some(size) = size.checked_next_multiple_of(STACK_ALIGN).filter(|&s| s <= isize::MAX as usize) else {
        return EINVAL;
    };
    unsafe { *attr = size as u64 | (*attr & ATTR_DETACHED) };
    0
}

#[no_mangle]
pub unsafe extern "C" fn pthread_attr_getstacksize(attr: *const u64, size: *mut usize) -> i32 {
    unsafe { *size = (*attr & !ATTR_DETACHED) as usize };
    0
}

#[no_mangle]
pub unsafe extern "C" fn pthread_attr_setdetachstate(attr: *mut u64, state: i32) -> i32 {
    let detached = match state {
        0 => 0,
        1 => ATTR_DETACHED,
        _ => return EINVAL,
    };
    unsafe { *attr = (*attr & !ATTR_DETACHED) | detached };
    0
}
