//! A memory object's pages are mapped where a caller says and shared, never
//! copied: a range of a region lands at its fixed place beside what was
//! there, a region somebody can write is never executed, and a program's
//! read-only segments are its file's own pages, which it holds no frame for
//! and can neither write nor be allowed to. Its writable segment is its own.
//!
//! Needs the guest: these are the kernel's answers at the system-call
//! boundary and the frames a real address space populates.

use std::io::Write;
use std::os::toyos::process::ExitStatusExt;
use std::process::{Command, Stdio};

use toyos::shm::SharedMemory;
use toyos::AsHandle;
use toyos_abi::end::{End, Fault};
use toyos_abi::syscall::{self, SyscallError, VmAt, VmProt, PAGE_SIZE};

const SELF_PATH: &str = "/system/bin/test_rs_vm_map_shares";
const STORES: &str = "stores";
/// The roles whose peaks are compared: reading no table, the read-only one,
/// the writable one.
const READS_NONE: &str = "reads-none";
const READS_SHARED: &str = "reads-shared";
const READS_PRIVATE: &str = "reads-private";

/// Words of each table: 64 pages.
const WORDS: usize = 64 * PAGE_SIZE / 8;

const fn table() -> [u64; WORDS] {
    let mut words = [0; WORDS];
    let mut i = 0;
    while i < WORDS {
        words[i] = (i as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        i += 1;
    }
    words
}

/// In a segment the program cannot write.
static READ_ONLY: [u64; WORDS] = table();
/// In its writable segment, with the same file bytes.
static mut WRITABLE: [u64; WORDS] = table();

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some(STORES) => stores(),
        Some(READS_NONE) => return,
        Some(READS_SHARED) => {
            assert_eq!(sum(READ_ONLY.as_ptr()), want(), "vm_map_shares: the read-only table is not the file's bytes");
            return;
        }
        Some(READS_PRIVATE) => {
            assert_eq!(sum(&raw const WRITABLE as *const u64), want(), "vm_map_shares: the writable table is not the file's bytes");
            return;
        }
        Some(other) => panic!("vm_map_shares: unknown role {other:?}"),
        None => {}
    }
    read_only_segments_are_the_files_pages();
    a_range_lands_at_its_place();
    a_writable_region_is_never_executed();
    an_image_is_executed_and_never_written();
    println!("vm_map_shares: an object's pages are mapped where asked, and a program's read-only segments are its file's own");
}

/// The most private memory a run of this program in `role` held, in bytes,
/// read off its end through the handle its spawn answers: a process's own
/// handle carries no `READ`, so none reads its own.
fn peak_of(role: &str) -> u64 {
    let image = toyos::files::read_image(SELF_PATH.as_bytes()).expect("vm_map_shares: its own image");
    let argv = format!("{SELF_PATH}\0{role}\0");
    let request = toyos::spawn::Spawn {
        program: SELF_PATH,
        image: None,
        image_held: true,
        argv: argv.as_bytes(),
        env: b"",
        cwd: "/",
        fds: &[],
        endow: Vec::new(),
        provided: Vec::new(),
        parent: toyos::spawn::Parent::Caller,
        independent: false,
        launcher: None,
        pager: None,
    };
    let Ok(toyos::spawn::Start::Helper(helper)) = request.start() else { panic!("vm_map_shares: a spawn of its own image is no helper") };
    let reader = helper.spawn(Some(&image)).expect("vm_map_shares: a reader starts");
    assert_eq!(reader.wait(), Ok(End::Exited(0)), "vm_map_shares: the {role} reader");
    reader.stats().expect("vm_map_shares: a reader's statistics").peak_memory
}

/// One word of each page of a table of this program's.
fn sum(words: *const u64) -> u64 {
    // SAFETY: `WORDS` words of a table of this program's.
    (0..WORDS).step_by(PAGE_SIZE / 8).fold(0u64, |sum, i| sum.wrapping_add(unsafe { words.add(i).read_volatile() }))
}

/// What [`sum`] reads of either table: the file's bytes.
fn want() -> u64 {
    (0..WORDS).step_by(PAGE_SIZE / 8).fold(0u64, |sum, i| sum.wrapping_add((i as u64 + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15)))
}

/// The middle two pages of a four-page region replace the middle two of a
/// reservation, which keeps its first and last; a range that is no whole
/// pages inside the region is refused.
fn a_range_lands_at_its_place() {
    let region = SharedMemory::create(4 * PAGE_SIZE).expect("vm_map_shares: a region");
    for page in 0..4 {
        region.bytes().write(page * PAGE_SIZE, &[page as u8 + 1; 8]);
    }
    let handle = region.as_handle().raw();
    // SAFETY: placed anywhere, so it replaces nothing.
    let at = unsafe { syscall::vm_reserve(4 * PAGE_SIZE, VmProt::ReadWrite, VmAt::ANYWHERE) }.expect("vm_map_shares: a reservation");
    // SAFETY: the reservation's own pages, each written before it is read.
    unsafe {
        at.write_volatile(0xaa);
        at.add(3 * PAGE_SIZE).write_volatile(0xbb);
    }
    // SAFETY: pages of the reservation nothing else reaches.
    let mapped = unsafe { syscall::vm_map(handle, PAGE_SIZE as u64, 2 * PAGE_SIZE, VmProt::Read, VmAt::Fixed(at.add(PAGE_SIZE))) };
    assert_eq!(mapped, Ok(unsafe { at.add(PAGE_SIZE) }), "vm_map_shares: a fixed mapping answered another place");
    // SAFETY: four mapped pages, read a byte each.
    let seen: Vec<u8> = (0..4).map(|page| unsafe { at.add(page * PAGE_SIZE).read_volatile() }).collect();
    assert_eq!(seen, [0xaa, 2, 3, 0xbb], "vm_map_shares: a range of a region beside the pages it left");
    region.bytes().write(2 * PAGE_SIZE, &[0x77]);
    // SAFETY: a mapped page.
    assert_eq!(unsafe { at.add(2 * PAGE_SIZE).read_volatile() }, 0x77, "vm_map_shares: a mapping is a copy of its region");
    for (offset, len) in [(1, PAGE_SIZE), (0, 5 * PAGE_SIZE), (4 * PAGE_SIZE as u64, PAGE_SIZE), (u64::MAX - 4095, PAGE_SIZE), (0, 0)] {
        // SAFETY: a refused map makes no mapping.
        let refused = unsafe { syscall::vm_map(handle, offset, len, VmProt::Read, VmAt::ANYWHERE) }.map(|_| ());
        assert_eq!(refused, Err(SyscallError::InvalidArgument), "vm_map_shares: {len:#x} bytes at {offset:#x} of a four-page region");
    }
}

fn a_writable_region_is_never_executed() {
    let region = SharedMemory::create(PAGE_SIZE).expect("vm_map_shares: a region");
    // SAFETY: a refused map makes no mapping.
    let executable = unsafe { syscall::vm_map(region.as_handle().raw(), 0, PAGE_SIZE, VmProt::ReadExec, VmAt::ANYWHERE) }.map(|_| ());
    assert_eq!(executable, Err(SyscallError::InvalidArgument), "vm_map_shares: a region its holder writes mapped executable");
}

/// This program's own file, held where nobody can write it, maps executable
/// and reads as the file; no mapping of it is writable, made so or widened.
fn an_image_is_executed_and_never_written() {
    let image = toyos::files::read_image(SELF_PATH.as_bytes()).expect("vm_map_shares: its own image");
    let held = image.as_immutable().expect("vm_map_shares: a /system program's image is one nobody can write");
    let handle = held.as_handle().raw();
    // SAFETY: placed anywhere, and never taken away.
    let at = unsafe { syscall::vm_map(handle, 0, PAGE_SIZE, VmProt::ReadExec, VmAt::ANYWHERE) }.expect("vm_map_shares: an image mapped executable");
    // SAFETY: a mapped page.
    let magic = unsafe { core::slice::from_raw_parts(at, 4) };
    assert_eq!(magic, b"\x7fELF", "vm_map_shares: an image's first page is not its file's");
    // SAFETY: a refused map makes no mapping.
    let writable = unsafe { syscall::vm_map(handle, 0, PAGE_SIZE, VmProt::ReadWrite, VmAt::ANYWHERE) }.map(|_| ());
    assert_eq!(writable, Err(SyscallError::PermissionDenied), "vm_map_shares: an image mapped writable");
    // SAFETY: the mapping above, which nothing else names.
    let widened = unsafe { syscall::vm_protect(at, PAGE_SIZE, VmProt::ReadWrite) };
    assert_eq!(widened, Err(SyscallError::PermissionDenied), "vm_map_shares: vm_protect made an image's mapping writable");
}

/// Reading every page of the read-only table takes no frame of this
/// process's own, and no call makes a page of it writable; reading the
/// writable table, the same bytes in the file, takes a frame a page.
fn read_only_segments_are_the_files_pages() {
    let want = want();
    let table_bytes = (WORDS * 8) as u64;

    let none = peak_of(READS_NONE);
    let shared = peak_of(READS_SHARED).saturating_sub(none);
    assert!(shared < table_bytes / 8, "vm_map_shares: reading {table_bytes} read-only bytes took {shared} bytes of private memory");

    let page = (READ_ONLY.as_ptr() as usize).next_multiple_of(PAGE_SIZE) as *mut u8;
    for prot in [VmProt::ReadWrite, VmProt::ReadExec] {
        // SAFETY: a refused change changes nothing.
        let widened = unsafe { syscall::vm_protect(page, PAGE_SIZE, prot) };
        assert_eq!(widened, Err(SyscallError::PermissionDenied), "vm_map_shares: vm_protect gave a shared page {prot:?}");
    }

    let private = peak_of(READS_PRIVATE).saturating_sub(none);
    assert!(private >= table_bytes * 3 / 4, "vm_map_shares: reading {table_bytes} writable bytes took {private} bytes of private memory");

    let child = Command::new(SELF_PATH).arg(STORES).stdout(Stdio::piped()).spawn().expect("vm_map_shares: spawn the storer");
    let out = child.wait_with_output().expect("vm_map_shares: wait the storer");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "reached stores", "vm_map_shares: the storer never reached its store");
    assert_eq!(
        End::from_word(out.status.into_raw()),
        Some(End::Fault(Fault::Memory)),
        "vm_map_shares: a store into a read-only segment was not a memory fault"
    );
    assert_eq!(sum(READ_ONLY.as_ptr()), want, "vm_map_shares: another process's store reached the shared pages");
}

/// Stores into its read-only table: the file's page, which every process
/// running this program shares.
fn stores() -> ! {
    println!("reached {STORES}");
    std::io::stdout().flush().expect("vm_map_shares: flush the marker");
    // SAFETY: none is owed: the store is the fault this role exists to take.
    unsafe { (READ_ONLY.as_ptr() as *mut u64).add(PAGE_SIZE / 8).write_volatile(1) };
    panic!("vm_map_shares: a store into a read-only segment landed");
}
