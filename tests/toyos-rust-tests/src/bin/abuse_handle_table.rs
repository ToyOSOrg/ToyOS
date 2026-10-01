//! The handle-table cap must hold on every insertion path.
//!
//! There are three of them — plain open, `dup2` and `SYS_SPAWN`'s slot map — and
//! a table that grows past the cap on any one reaches a hashbrown doubling
//! above the kernel's 2 MiB single-allocation ceiling.
//!
//! The cap **is** the slot range now: a `RawHandle` carries twelve bits of
//! slot, so "the table is full" and "that slot does not exist" are one event
//! and one error word rather than two that could disagree.
//!
//! **`SYS_SPAWN`'s two vectors are bounded by their own counts as well, and
//! the cap cannot stand in for either.** A slot map that names one slot over
//! and over never trips the cap — the child's table stays one entry wide — and
//! an endowment vector that names one handle twice passes a preflight run
//! against a table nothing has been taken out of yet. Both were reachable with
//! one argument and both ended in the kernel rather than in an error word, so
//! each has an arm here and the last arm is what says the machine survived
//! them.
//!
//! **An endowment vector is one entry and one label short of a table**, which
//! the kernel's own `self` fills. So a caller's entry labelled `self` is
//! refused before anything moves, and so is a vector of `MAX_ENDOWMENTS`
//! entries, while one entry fewer starts.

use toyos_abi::syscall::{
    self, EndowEntry, MmapFlags, MmapProt, SpawnArgs, SyscallError, MAX_ENDOWMENTS, MAX_SLOT_MAP, SELF_LABEL,
};
use toyos_abi::RawHandle;

const SELF: &str = "/system/bin/test_rs_abuse_handle_table";
/// The role a spawned copy of this binary takes: it exits at once.
const CHILD: &str = "endowed";

/// Where the child starts: `SpawnArgs` names a working directory or the spawn is refused.
const CWD: &str = "/";

/// Mirrors `RawHandle::MAX_SLOTS`. A cap that moves should fail this test
/// loudly.
const MAX_HANDLES: u32 = 4096;

const REGION: usize = 4 * 1024 * 1024;
const PAIRS: usize = 100_000;

/// The label blob the endowment arms index into. Its content is never read —
/// the vector is refused before anything is installed — but the offsets have to
/// be in range or the refusal would be the label check rather than the repeat.
const LABELS: &[u8] = b"twice";

fn main() {
    if std::env::args().nth(1).as_deref() == Some(CHILD) {
        return;
    }
    let region = unsafe {
        syscall::mmap(
            core::ptr::null_mut(),
            REGION,
            MmapProt::READ | MmapProt::WRITE,
            MmapFlags::ANONYMOUS | MmapFlags::PRIVATE,
        )
    };
    assert!(!region.is_null(), "mmap failed");

    // The child's table is built by a different insert path than dup2's.
    let pairs = region as *mut [u32; 2];
    for i in 0..PAIRS {
        unsafe { pairs.add(i).write([i as u32, 1]) };
    }
    let argv = unsafe { region.add(REGION / 2) };
    const ARGV0: &[u8] = b"/system/bin/echo\0";
    unsafe { core::ptr::copy_nonoverlapping(ARGV0.as_ptr(), argv, ARGV0.len()) };

    let spawn_with = |slot_map_count: u64, endow_count: u64| {
        let args = SpawnArgs {
            argv_ptr: argv as u64,
            argv_len: ARGV0.len() as u64,
            slot_map_ptr: region as u64,
            slot_map_count,
            env_ptr: 0,
            env_len: 0,
            endow_ptr: (region as u64) + (REGION / 4) as u64,
            endow_count,
            labels_ptr: (region as u64) + (REGION / 8) as u64,
            labels_len: LABELS.len() as u64,
            cwd_ptr: CWD.as_ptr() as u64,
            cwd_len: CWD.len() as u64,
            image: 0,
            image_len: 0,
            place: u64::from(toyos_abi::HANDLE_INVALID.0),
        };
        unsafe { syscall::spawn(&args) }
    };

    // A slot map longer than the child's table has slots. Past this the count
    // is bounded only by the 2 MiB window the arguments are read through.
    let err = spawn_with(PAIRS as u64, 0)
        .expect_err("a slot_map past MAX_SLOT_MAP must be rejected");
    assert_eq!(err, SyscallError::InvalidArgument, "wrong error for an oversized slot_map");

    // **The same length, every pair naming slot 0.** The cap never fires: the
    // child's table stays one entry wide while the parent duplicates a handle
    // per pair under its own lock and carries every displaced entry out of it.
    for i in 0..PAIRS {
        unsafe { pairs.add(i).write([0, 1]) };
    }
    let err = spawn_with(PAIRS as u64, 0)
        .expect_err("a slot_map repeating one slot must be rejected");
    assert_eq!(err, SyscallError::InvalidArgument, "wrong error for a repeated slot");

    // And the cap itself, which is a different refusal and is what says the two
    // above are the *count* rather than a blanket no. One pair, naming a slot
    // no table has.
    unsafe { pairs.write([MAX_SLOT_MAP as u32 + 1, 1]) };
    let err = spawn_with(1, 0).expect_err("a slot past the table must be rejected");
    assert_eq!(err, SyscallError::ResourceExhausted, "wrong error at the child's table cap");

    // **An endowment vector naming one handle twice.** Every check in the
    // preflight runs against a table nothing has been removed from, so a repeat
    // passed both times; the first removal retired the slot and the second
    // answered `Stale` into an `expect`.
    let endow = unsafe { region.add(REGION / 4) } as *mut EndowEntry;
    let entry = EndowEntry { label_off: 0, label_len: LABELS.len() as u32, handle: RawHandle(1), _pad: 0 };
    unsafe {
        endow.write(entry);
        endow.add(1).write(entry);
        core::ptr::copy_nonoverlapping(
            LABELS.as_ptr(),
            region.add(REGION / 8),
            LABELS.len(),
        );
    }
    let err = spawn_with(0, 2).expect_err("an endowment vector naming one handle twice must be rejected");
    assert_eq!(err, SyscallError::InvalidArgument, "wrong error for a repeated endowment");

    a_caller_endows_no_self();
    a_spawn_carries_one_entry_fewer_than_a_table();

    // dup2 picks the slot, so it never went through the allocating path that
    // carried the cap.
    let mut refused = None;
    for n in 3..40_000u16 {
        if let Err(e) = syscall::dup2(RawHandle(1), n) {
            refused = Some((n, e));
            break;
        }
    }
    let (n, e) = refused.expect("dup2 must eventually refuse to grow the handle table");
    assert_eq!(e, SyscallError::ResourceExhausted, "wrong error at the handle cap");
    assert!(
        u32::from(n) <= MAX_HANDLES + 16,
        "handle table reached {n} slots, past the {MAX_HANDLES} cap"
    );

    // The cap is a live limit, not a latched failure. Every slot below `n` is
    // at generation 0, so its handle is the bare slot index.
    for slot in 3..n {
        syscall::close(RawHandle(u32::from(slot)));
    }
    let reused = syscall::dup2(RawHandle(1), 3)
        .expect("dup2 must work again after closing handles");
    syscall::close(reused);

    unsafe { syscall::munmap(region, REGION) }.expect("munmap");
    println!("handle table capped at {MAX_HANDLES} on every insert path (refused at {n})");
}

/// `SYS_SPAWN` of this binary as a child that exits at once, carrying
/// `entries` into the blob `labels`.
fn spawn_endowed(entries: &[EndowEntry], labels: &[u8]) -> Result<RawHandle, SyscallError> {
    let argv = format!("{SELF}\0{CHILD}");
    // SAFETY: every pointer names a live local for the whole call.
    unsafe {
        syscall::spawn(&SpawnArgs {
            argv_ptr: argv.as_ptr() as u64,
            argv_len: argv.len() as u64,
            slot_map_ptr: 0,
            slot_map_count: 0,
            env_ptr: 0,
            env_len: 0,
            endow_ptr: entries.as_ptr() as u64,
            endow_count: entries.len() as u64,
            labels_ptr: labels.as_ptr() as u64,
            labels_len: labels.len() as u64,
            cwd_ptr: CWD.as_ptr() as u64,
            cwd_len: CWD.len() as u64,
            image: 0,
            image_len: 0,
            place: u64::from(toyos_abi::HANDLE_INVALID.0),
        })
    }
}

/// An entry labelled `self` would shadow the kernel's in the child's lookup:
/// refused before anything moves, so the pipe end it named is still this
/// process's and still carries a byte.
fn a_caller_endows_no_self() {
    let ends = syscall::pipe().expect("a pipe for the self arm");
    let entry = EndowEntry { label_off: 0, label_len: SELF_LABEL.len() as u32, handle: ends.write, _pad: 0 };
    let err = spawn_endowed(&[entry], SELF_LABEL.as_bytes()).expect_err("an endowment labelled self must be refused");
    assert_eq!(err, SyscallError::InvalidArgument, "wrong error for an endowment labelled self");
    assert_eq!(syscall::write(ends.write, b"x"), Ok(1), "the end an endowment labelled self named is gone");
    let mut byte = [0u8; 1];
    assert_eq!(syscall::read(ends.read, &mut byte), Ok(1), "the byte did not arrive");
    syscall::close(ends.read);
    syscall::close(ends.write);
}

/// `MAX_ENDOWMENTS` entries naming distinct `TRANSFER` handles, so the count
/// is the only refusal: refused, and one fewer starts.
fn a_spawn_carries_one_entry_fewer_than_a_table() {
    let mut handles = Vec::with_capacity(MAX_ENDOWMENTS);
    while handles.len() < MAX_ENDOWMENTS {
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
    let err = spawn_endowed(&entries, &labels).expect_err("a spawn carrying MAX_ENDOWMENTS entries must be refused");
    assert_eq!(err, SyscallError::InvalidArgument, "wrong error for MAX_ENDOWMENTS entries");

    let fewer = MAX_ENDOWMENTS - 1;
    let used = entries[fewer].label_off as usize;
    let child = spawn_endowed(&entries[..fewer], &labels[..used])
        .unwrap_or_else(|e| panic!("a spawn carrying {fewer} entries was refused: {e:?}"));
    assert_eq!(syscall::process_wait(child), Ok(0), "the child carrying {fewer} entries");
    syscall::close(child);
    syscall::close(handles[fewer]);
    println!("an endowment labelled self and {MAX_ENDOWMENTS} entries are refused, and {fewer} start");
}
