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
//! **A child's table starts in the order the ABI states**: the kernel's own
//! two, then the endowments as the spawn listed them, the `i`th at
//! `RawHandle::endowed(i)`, which is all a child finds them by. A vector of
//! one entry more than `MAX_SPAWN_ENDOWMENTS` is refused before anything
//! moves, while that many start.
//!
//! **A pipe and a port are two inserts answered as one**: at the cap, and one
//! slot short of fitting, each is refused with the table as it was.

#[path = "../image.rs"]
mod image;

use toyos_abi::syscall::{self, VmAt, VmProt, SpawnArgs, SyscallError, MAX_SPAWN_ENDOWMENTS};
use toyos_abi::handle::TransferEntry;
use toyos_abi::RawHandle;
use toyos_abi::end::End;

const SELF: &str = "/system/bin/test_rs_abuse_handle_table";
/// The name its children go by.
const NAME: &str = "test_rs_abuse_handle_table";
/// The role a spawned copy of this binary takes: it writes its place's
/// digit into each of the pipe ends it was endowed, and exits.
const CHILD: &str = "endowed";


/// Mirrors `RawHandle::MAX_SLOTS`. A cap that moves should fail this test
/// loudly.
const MAX_HANDLES: u32 = 4096;

const REGION: usize = 4 * 1024 * 1024;

/// This binary laid out to start as [`CHILD`], read before the table is
/// filled: a full table has no slot for the objects a read and a startup
/// block make.
fn own() -> &'static image::Laid {
    // The image is kept: the ranges name its object.
    static OWN: std::sync::OnceLock<(toyos::process::Image, image::Laid)> = std::sync::OnceLock::new();
    let (_, laid) = OWN.get_or_init(|| {
        let image = image::image_of(SELF);
        let laid = image::laid(&image, &format!("{SELF}\0{CHILD}\0{PROMISED}"));
        (image, laid)
    });
    laid
}

fn main() {
    if let Some(count) = std::env::args().nth(2).filter(|_| std::env::args().nth(1).as_deref() == Some(CHILD)) {
        for i in 0..count.parse::<usize>().expect("a count of endowments") {
            // The `i`th endowment is the `i`th handle after the kernel's own,
            // whatever the child's own SDK makes of its table.
            let digit = [b'0' + i as u8];
            assert_eq!(syscall::write(RawHandle::endowed(i), &digit), Ok(1), "endowment {i} is no pipe's write end");
        }
        return;
    }
    own();
    let region = unsafe {
        syscall::vm_reserve(REGION, VmProt::ReadWrite, VmAt::ANYWHERE).unwrap_or(core::ptr::null_mut())
    };
    assert!(!region.is_null(), "mmap failed");

    let path = unsafe { region.add(REGION / 2) };
    const PATH: &str = "/system/bin/echo";
    unsafe { core::ptr::copy_nonoverlapping(PATH.as_ptr(), path, PATH.len()) };

    let echo = image::image_of("/system/bin/echo");
    let laid = image::laid(&echo, PATH);
    let spawn_with = |endow_count: u64| {
        let args = SpawnArgs {
            path_ptr: path as u64,
            path_len: PATH.len() as u64,
            name_ptr: NAME.as_ptr() as u64,
            name_len: NAME.len() as u64,
            endow_ptr: (region as u64) + (REGION / 4) as u64,
            endow_count,
            maps_ptr: laid.maps().as_ptr() as u64,
            maps_count: laid.maps().len() as u64,
            entry: laid.entry,
            image_base: laid.image_base,
            build_id_ptr: laid.build_id().as_ptr() as u64,
            build_id_len: laid.build_id().len() as u64,
            place: u64::from(toyos_abi::HANDLE_INVALID.0),
            job: u64::from(toyos_abi::HANDLE_INVALID.0),
            pager: u64::from(toyos_abi::HANDLE_INVALID.0),
            isolated_speculation: 0,
        };
        unsafe { syscall::spawn(&args) }
    };

    // **An endowment vector naming one handle twice.** Every check in the
    // preflight runs against a table nothing has been removed from, so a repeat
    // passed both times; the first removal retired the slot and the second
    // answered `Stale` into an `expect`.
    let endow = unsafe { region.add(REGION / 4) } as *mut TransferEntry;
    let entry = TransferEntry::unchanged(stdout());
    unsafe {
        endow.write(entry);
        endow.add(1).write(entry);
    }
    let err = spawn_with(2).expect_err("an endowment vector naming one handle twice must be rejected");
    assert_eq!(err, SyscallError::InvalidArgument, "wrong error for a repeated endowment");

    a_child_finds_each_endowment_at_its_place();
    a_spawn_carries_its_bound();

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
    let spawned = spawn_endowed(&[TransferEntry::unchanged(last)]);

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

/// `SYS_SPAWN` of this binary as [`CHILD`], carrying `entries`.
fn spawn_endowed(entries: &[TransferEntry]) -> Result<RawHandle, SyscallError> {
    let laid = own();
    // SAFETY: every pointer names a live local for the whole call.
    unsafe {
        syscall::spawn(&SpawnArgs {
            path_ptr: SELF.as_ptr() as u64,
            path_len: SELF.len() as u64,
            name_ptr: NAME.as_ptr() as u64,
            name_len: NAME.len() as u64,
            endow_ptr: entries.as_ptr() as u64,
            endow_count: entries.len() as u64,
            maps_ptr: laid.maps().as_ptr() as u64,
            maps_count: laid.maps().len() as u64,
            entry: laid.entry,
            image_base: laid.image_base,
            build_id_ptr: laid.build_id().as_ptr() as u64,
            build_id_len: laid.build_id().len() as u64,
            place: u64::from(toyos_abi::HANDLE_INVALID.0),
            job: u64::from(toyos_abi::HANDLE_INVALID.0),
            pager: u64::from(toyos_abi::HANDLE_INVALID.0),
            isolated_speculation: 0,
        })
    }
}

/// How many endowments the laid-out [`CHILD`] writes its digit into.
const PROMISED: usize = 3;

/// A raw spawn's child finds its `i`th endowment at `RawHandle::endowed(i)`:
/// three pipe ends, each written by the child at the place the ABI states,
/// each byte read from its own pipe.
fn a_child_finds_each_endowment_at_its_place() {
    let pipes: Vec<_> = (0..PROMISED).map(|_| syscall::pipe().expect("a pipe for the place arm")).collect();
    let entries: Vec<TransferEntry> = pipes.iter().map(|ends| TransferEntry::unchanged(ends.write)).collect();
    let child = spawn_endowed(&entries).unwrap_or_else(|e| panic!("a spawn carrying {PROMISED} pipe ends was refused: {e:?}"));
    assert_eq!(syscall::process_wait(child), Ok(End::Exited(0)), "the child that writes its endowments");
    for (i, ends) in pipes.iter().enumerate() {
        let mut byte = [0u8; 1];
        assert_eq!(syscall::read(ends.read, &mut byte), Ok(1), "endowment {i} carried nothing");
        assert_eq!(byte[0], b'0' + i as u8, "endowment {i} was found at another place");
        // SAFETY: no type holds this number.
        unsafe { syscall::close(ends.read) };
    }
    // SAFETY: no type holds this number.
    unsafe { syscall::close(child) };
}

/// One entry more than `MAX_SPAWN_ENDOWMENTS`, each naming a distinct
/// `TRANSFER` handle, so the count is the only refusal: refused, and one fewer
/// starts, the first [`PROMISED`] pipe write ends.
fn a_spawn_carries_its_bound() {
    let over = MAX_SPAWN_ENDOWMENTS + 1;
    let mut handles = Vec::with_capacity(over + 1);
    while handles.len() < over {
        let ends = syscall::pipe().expect("a pipe for the count arm");
        handles.extend([ends.write, ends.read]);
    }
    let entries: Vec<TransferEntry> = handles[..over].iter().map(|&handle| TransferEntry::unchanged(handle)).collect();
    let err = spawn_endowed(&entries).expect_err("a spawn carrying an entry past MAX_SPAWN_ENDOWMENTS must be refused");
    assert_eq!(err, SyscallError::InvalidArgument, "wrong error for {over} entries");

    // The child writes into its first `PROMISED`: write ends, in that order.
    let fewer = MAX_SPAWN_ENDOWMENTS;
    let ordered: Vec<TransferEntry> = handles.iter().step_by(2).chain(handles.iter().skip(1).step_by(2)).take(fewer).map(|&h| TransferEntry::unchanged(h)).collect();
    let child = spawn_endowed(&ordered).unwrap_or_else(|e| panic!("a spawn carrying {fewer} entries was refused: {e:?}"));
    assert_eq!(syscall::process_wait(child), Ok(End::Exited(0)), "the child carrying {fewer} entries");
    // SAFETY: no type holds this number.
    unsafe { syscall::close(child) };
    for &handle in &handles {
        if !ordered.iter().any(|moved| moved.handle == handle) {
            // SAFETY: no type holds this number.
            unsafe { syscall::close(handle) };
        }
    }
    println!("{over} entries are refused, {fewer} start, and each lands at the place the ABI states");
}

/// The kernel handle this process's standard output is, descriptor 1's.
fn stdout() -> RawHandle {
    toyos::fd::with(1, |o| o.handle().map(toyos::BorrowedHandle::raw)).ok().flatten().expect("the test is given a standard output")
}
