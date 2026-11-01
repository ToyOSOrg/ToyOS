//! The handle-table cap must hold on every insertion path.
//!
//! There are two of them — a plain install, as an open or a `dup` makes, and
//! `SYS_SPAWN`'s endowments — and a table that grows past the cap on either
//! reaches a hashbrown doubling above the kernel's 2 MiB single-allocation
//! ceiling.
//!
//! The cap **is** the slot range now: a `RawHandle` carries twelve bits of
//! slot, so "the table is full" and "that slot does not exist" are one event
//! and one error word rather than two that could disagree.
//!
//! **`SYS_SPAWN`'s endowments are bounded by their own count as well, and
//! the cap cannot stand in for it.** An endowment vector that names one handle
//! twice passes a preflight run against a table nothing has been taken out of
//! yet. It was reachable with one argument and ended in the kernel rather than
//! in an error word, so it has an arm here and the last arm is what says the
//! machine survived it.
//!
//! **A spawn's answer is an insert into its caller's table**, whose room is
//! checked before the spawn's endowments move: at the cap the spawn is
//! refused with every handle it named still the caller's.
//!
//! **An endowment vector is two entries and two labels short of a table**,
//! which the kernel's own `self` and `quit` fill. So a caller's entry labelled
//! either is refused before anything moves, and so is a vector of one entry
//! more than `MAX_SPAWN_ENDOWMENTS`, while that many start.
//!
//! **A pipe and a port are two inserts answered as one**: at the cap, and one
//! slot short of fitting, each is refused with the table as it was.

#[path = "../image.rs"]
mod image;

use toyos_abi::syscall::{
    self, EndowEntry, VmAt, VmProt, SpawnArgs, SyscallError, MAX_SPAWN_ENDOWMENTS, QUIT_LABEL, SELF_LABEL,
};
use toyos_abi::RawHandle;
use toyos_abi::end::End;

const SELF: &str = "/system/bin/test_rs_abuse_handle_table";
/// The role a spawned copy of this binary takes: it exits at once.
const CHILD: &str = "endowed";

/// Where the child starts: `SpawnArgs` names a working directory or the spawn is refused.
const CWD: &str = "/";

/// Mirrors `RawHandle::MAX_SLOTS`. A cap that moves should fail this test
/// loudly.
const MAX_HANDLES: u32 = 4096;

const REGION: usize = 4 * 1024 * 1024;

/// The label blob the endowment arms index into. Its content is never read —
/// the vector is refused before anything is installed — but the offsets have to
/// be in range or the refusal would be the label check rather than the repeat.
const LABELS: &[u8] = b"twice";

/// This binary as an image, read before the table is filled: a full table has
/// no slot for the object a read makes.
fn own() -> &'static toyos::process::Image {
    static OWN: std::sync::OnceLock<toyos::process::Image> = std::sync::OnceLock::new();
    OWN.get_or_init(|| image::image_of(SELF))
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some(CHILD) {
        return;
    }
    own();
    let region = unsafe {
        syscall::vm_reserve(REGION, VmProt::ReadWrite, VmAt::ANYWHERE).unwrap_or(core::ptr::null_mut())
    };
    assert!(!region.is_null(), "mmap failed");

    let argv = unsafe { region.add(REGION / 2) };
    const ARGV0: &[u8] = b"/system/bin/echo\0";
    unsafe { core::ptr::copy_nonoverlapping(ARGV0.as_ptr(), argv, ARGV0.len()) };

    let echo = image::image_of("/system/bin/echo");
    let (image, image_len) = echo.spawn_words();
    let spawn_with = |endow_count: u64| {
        let args = SpawnArgs {
            path_ptr: argv as u64,
            path_len: (ARGV0.len() - 1) as u64,
            argv_ptr: argv as u64,
            argv_len: ARGV0.len() as u64,
            env_ptr: 0,
            env_len: 0,
            endow_ptr: (region as u64) + (REGION / 4) as u64,
            endow_count,
            labels_ptr: (region as u64) + (REGION / 8) as u64,
            labels_len: LABELS.len() as u64,
            cwd_ptr: CWD.as_ptr() as u64,
            cwd_len: CWD.len() as u64,
            image,
            image_len,
            place: u64::from(toyos_abi::HANDLE_INVALID.0),
            pager: u64::from(toyos_abi::HANDLE_INVALID.0),
        };
        unsafe { syscall::spawn(&args) }
    };

    // **An endowment vector naming one handle twice.** Every check in the
    // preflight runs against a table nothing has been removed from, so a repeat
    // passed both times; the first removal retired the slot and the second
    // answered `Stale` into an `expect`.
    let endow = unsafe { region.add(REGION / 4) } as *mut EndowEntry;
    let entry = EndowEntry { label_off: 0, label_len: LABELS.len() as u32, handle: stdout(), _pad: 0 };
    unsafe {
        endow.write(entry);
        endow.add(1).write(entry);
        core::ptr::copy_nonoverlapping(
            LABELS.as_ptr(),
            region.add(REGION / 8),
            LABELS.len(),
        );
    }
    let err = spawn_with(2).expect_err("an endowment vector naming one handle twice must be rejected");
    assert_eq!(err, SyscallError::InvalidArgument, "wrong error for a repeated endowment");

    a_caller_endows_no_self();
    a_spawn_carries_two_entries_fewer_than_a_table();

    let mut filled = Vec::new();
    let mut refused = None;
    for n in 0..40_000u32 {
        // SAFETY: nothing relies on being the source's only holder.
        match unsafe { syscall::dup(stdout()) } {
            Ok(handle) => filled.push(handle),
            Err(e) => {
                refused = Some((n, e));
                break;
            }
        }
    }
    let (n, e) = refused.expect("dup must eventually refuse to grow the handle table");
    assert_eq!(e, SyscallError::ResourceExhausted, "wrong error at the handle cap");
    assert!(n <= MAX_HANDLES, "the handle table took {n} more handles, past the {MAX_HANDLES} cap");

    a_pair_is_refused_whole(&mut filled);

    let last = filled.pop().expect("the fill installed a handle");
    let entry = EndowEntry { label_off: 0, label_len: LABELS.len() as u32, handle: last, _pad: 0 };
    let spawned = spawn_endowed(&[entry], LABELS);

    // The cap is a live limit, not a latched failure.
    for handle in filled {
        // SAFETY: no type holds this number.
        unsafe { syscall::close(handle) };
    }
    // Judged once the table has room again: a panic at the cap aborts with no report.
    assert_eq!(spawned, Err(SyscallError::ResourceExhausted), "a spawn from a full table was not refused");
    // Still this process's: the close ends it if the refused spawn moved it.
    // SAFETY: no type holds this number.
    unsafe { syscall::close(last) };
    // SAFETY: nothing relies on being the source's only holder.
    let reused = unsafe { syscall::dup(stdout()) }.expect("dup must work again after closing handles");
    // SAFETY: no type holds this number.
    unsafe { syscall::close(reused) };

    unsafe { syscall::vm_release(region, REGION) }.expect("munmap");
    println!("handle table capped at {MAX_HANDLES} on every insert path (refused at {n})");
}

/// A pipe and a port are two handles each: refused at the cap, and refused
/// with one slot free, which is then still free. Takes a full table and
/// leaves it full.
fn a_pair_is_refused_whole(filled: &mut Vec<RawHandle>) {
    let refused = |room: &str| {
        assert_eq!(
            syscall::pipe().err(),
            Some(SyscallError::ResourceExhausted),
            "a pipe with {room}"
        );
        assert_eq!(
            syscall::port_create().err(),
            Some(SyscallError::ResourceExhausted),
            "a port with {room}"
        );
    };
    refused("no slot free");
    // SAFETY: no type holds this number.
    unsafe { syscall::close(filled.pop().expect("the fill installed a handle")) };
    refused("one slot free");
    // SAFETY: nothing relies on being the source's only holder.
    filled.push(unsafe { syscall::dup(stdout()) }.expect("the one free slot survived two refused pairs"));
    assert_eq!(
        // SAFETY: nothing relies on being the source's only holder.
        unsafe { syscall::dup(stdout()) },
        Err(SyscallError::ResourceExhausted),
        "a refused pair left a second slot free"
    );
}

/// `SYS_SPAWN` of this binary as a child that exits at once, carrying
/// `entries` into the blob `labels`.
fn spawn_endowed(entries: &[EndowEntry], labels: &[u8]) -> Result<RawHandle, SyscallError> {
    let argv = format!("{SELF}\0{CHILD}");
    let (image, image_len) = own().spawn_words();
    // SAFETY: every pointer names a live local for the whole call.
    unsafe {
        syscall::spawn(&SpawnArgs {
            path_ptr: SELF.as_ptr() as u64,
            path_len: SELF.len() as u64,
            argv_ptr: argv.as_ptr() as u64,
            argv_len: argv.len() as u64,
            env_ptr: 0,
            env_len: 0,
            endow_ptr: entries.as_ptr() as u64,
            endow_count: entries.len() as u64,
            labels_ptr: labels.as_ptr() as u64,
            labels_len: labels.len() as u64,
            cwd_ptr: CWD.as_ptr() as u64,
            cwd_len: CWD.len() as u64,
            image,
            image_len,
            place: u64::from(toyos_abi::HANDLE_INVALID.0),
            pager: u64::from(toyos_abi::HANDLE_INVALID.0),
        })
    }
}

/// An entry labelled `self` or `quit` would shadow the kernel's in the
/// child's lookup: refused before anything moves, so the pipe end it named is
/// still this process's and still carries a byte.
fn a_caller_endows_no_self() {
    for label in [SELF_LABEL, QUIT_LABEL] {
        let ends = syscall::pipe().expect("a pipe for the self arm");
        let entry = EndowEntry { label_off: 0, label_len: label.len() as u32, handle: ends.write, _pad: 0 };
        let err = spawn_endowed(&[entry], label.as_bytes()).expect_err("an endowment labelled as the kernel's must be refused");
        assert_eq!(err, SyscallError::InvalidArgument, "wrong error for an endowment labelled {label}");
        assert_eq!(syscall::write(ends.write, b"x"), Ok(1), "the end an endowment labelled {label} named is gone");
        let mut byte = [0u8; 1];
        assert_eq!(syscall::read(ends.read, &mut byte), Ok(1), "the byte did not arrive");
        // SAFETY: no type holds this number.
        unsafe { syscall::close(ends.read) };
        // SAFETY: no type holds this number.
        unsafe { syscall::close(ends.write) };
    }
}

/// One entry more than `MAX_SPAWN_ENDOWMENTS`, each naming a distinct
/// `TRANSFER` handle, so the count is the only refusal: refused, and one fewer
/// starts.
fn a_spawn_carries_two_entries_fewer_than_a_table() {
    let over = MAX_SPAWN_ENDOWMENTS + 1;
    let mut handles = Vec::with_capacity(over + 1);
    while handles.len() < over {
        let ends = syscall::pipe().expect("a pipe for the count arm");
        handles.extend([ends.read, ends.write]);
    }
    let mut labels = Vec::new();
    let entries: Vec<EndowEntry> = handles
        .iter()
        .enumerate()
        .map(|(i, &handle)| {
            let label = format!("e{i:02}");
            let entry =
                EndowEntry { label_off: labels.len() as u32, label_len: label.len() as u32, handle, _pad: 0 };
            labels.extend_from_slice(label.as_bytes());
            entry
        })
        .collect();
    let entries = &entries[..over];
    let err = spawn_endowed(entries, &labels).expect_err("a spawn carrying an entry past MAX_SPAWN_ENDOWMENTS must be refused");
    assert_eq!(err, SyscallError::InvalidArgument, "wrong error for {over} entries");

    let fewer = MAX_SPAWN_ENDOWMENTS;
    let used = entries[fewer].label_off as usize;
    let child = spawn_endowed(&entries[..fewer], &labels[..used])
        .unwrap_or_else(|e| panic!("a spawn carrying {fewer} entries was refused: {e:?}"));
    assert_eq!(syscall::process_wait(child), Ok(End::Exited(0)), "the child carrying {fewer} entries");
    // SAFETY: no type holds this number.
    unsafe { syscall::close(child) };
    for &handle in &handles[fewer..] {
        // SAFETY: no type holds this number.
        unsafe { syscall::close(handle) };
    }
    println!("an endowment labelled self or quit and {over} entries are refused, and {fewer} start");
}

/// The kernel handle this process's standard output is, descriptor 1's.
fn stdout() -> RawHandle {
    toyos::fd::with(1, |o| o.handle().map(toyos::BorrowedHandle::raw)).ok().flatten().expect("the test is given a standard output")
}
