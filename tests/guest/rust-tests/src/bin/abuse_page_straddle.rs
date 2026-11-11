//! A typed syscall argument that crosses a page boundary is read and written
//! whole. The two pages of a mapping are two frames wherever the allocator put
//! them, so a value placed across the boundary is a copy cut at the seam: one
//! translated as a single run would read or write its tail in whatever frame
//! the allocator handed out after the first, and one refused would refuse a
//! value its own process may store.
//!
//! Two are reachable from userland: `fstat`'s `Stat`, a kernel write, and
//! `spawn`'s `SpawnArgs`, a kernel read whose `path_ptr` and ranges the
//! kernel then acts on.
//!
//! **The verdict is the bytes**: each straddling value is read back field by
//! field on both sides of the boundary, and a straddling `SpawnArgs` spawns.

#[path = "../image.rs"]
mod image;

use toyos_abi::RawHandle;
use toyos_abi::syscall::{
    self, VmAt, VmProt, ProcessStats, SpawnArgs, Stat, SyscallError, SYS_FSTAT,
    SYS_PROCESS_STATS,
};


const PAGE: u64 = 4096;
/// The name its children go by.
const NAME: &str = "echo";
const STAT_LEN: usize = core::mem::size_of::<Stat>();
const CANARY: u8 = 0xA5;

/// Every typed wrapper fills the value on its own stack, so none of them can
/// express the argument under test. Number in rdi, arguments in rsi/rdx/r8/r9.
fn raw(num: u64, a1: u64, a2: u64, a3: u64) -> u64 {
    let ret: u64;
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rdi") num,
            in("rsi") a1,
            in("rdx") a2,
            in("r8") a3,
            in("r9") 0u64,
            lateout("rax") ret,
            out("rcx") _,
            out("r11") _,
        );
    }
    ret
}

fn fstat_raw(handle: RawHandle, out: u64) -> u64 {
    raw(SYS_FSTAT, handle.0 as u64, out, 0)
}

fn err(ret: u64) -> Option<SyscallError> {
    SyscallError::from_u64(ret)
}

fn main() {
    let region = unsafe {
        syscall::vm_reserve(2 * PAGE as usize, VmProt::ReadWrite, VmAt::ANYWHERE).unwrap_or(core::ptr::null_mut())
    };
    assert!(!region.is_null(), "mmap failed");
    let base = region as u64;
    let boundary = base + PAGE;

    let handle = syscall::null(toyos_abi::Rights::READ).expect("a generic object for fstat");
    let poison = |addr: u64, len: usize| {
        for i in 0..len {
            unsafe { (addr as *mut u8).add(i).write_volatile(CANARY) };
        }
    };

    // 1. A `Stat` eight bytes below the boundary, against one wholly inside the
    //    first page: the kernel answers both alike.
    let fitting = boundary - 2 * STAT_LEN as u64;
    assert!(err(fstat_raw(handle, fitting)).is_none(), "fstat refused a Stat inside one page");
    let want = unsafe { (fitting as *const [u8; STAT_LEN]).read_volatile() };
    let straddling = boundary - 8;
    poison(straddling, STAT_LEN);
    let ret = fstat_raw(handle, straddling);
    assert!(err(ret).is_none(), "fstat refused a Stat across a page boundary: {ret:#x}");
    let got = unsafe { (straddling as *const [u8; STAT_LEN]).read_volatile() };
    assert_eq!(got, want, "fstat wrote a different Stat across the boundary than inside one page");
    // SAFETY: no type holds this number.
    unsafe { syscall::close(handle) };

    // 2. `SpawnArgs` straddling, read whole: the child it names starts.
    const PATH: &str = "/system/bin/echo";
    let echo = image::image_of("/system/bin/echo");
    let laid = image::laid(&echo, "/system/bin/echo\0straddle");
    unsafe {
        core::ptr::copy_nonoverlapping(PATH.as_ptr(), base as *mut u8, PATH.len());
    }
    let args = SpawnArgs {
        path_ptr: base,
        path_len: PATH.len() as u64,
        name_ptr: NAME.as_ptr() as u64,
        name_len: NAME.len() as u64,
        endow_ptr: 0,
        endow_count: 0,
        maps_ptr: laid.maps().as_ptr() as u64,
        maps_count: laid.maps().len() as u64,
        entry: laid.entry,
        image_base: laid.image_base,
        build_id_ptr: laid.build_id().as_ptr() as u64,
        build_id_len: laid.build_id().len() as u64,
        place: u64::from(toyos_abi::HANDLE_INVALID.0),
        job: u64::from(toyos_abi::HANDLE_INVALID.0),
        pager: u64::from(toyos_abi::HANDLE_INVALID.0),
    };
    let placed = (boundary - 8) as *mut SpawnArgs;
    unsafe { placed.write_volatile(args) };
    let child = unsafe { syscall::spawn(&*placed) }.expect("spawn from a SpawnArgs across a page boundary");
    syscall::process_wait(child).expect("wait for the child");

    // 3. `ProcessStats`, which the kernel fills, across the boundary.
    let stats_len = core::mem::size_of::<ProcessStats>();
    let straddling = boundary - 8;
    poison(straddling, stats_len);
    let ret = raw(SYS_PROCESS_STATS, child.0 as u64, straddling, stats_len as u64);
    assert!(err(ret).is_none(), "process_stats refused a ProcessStats across a page boundary: {ret:#x}");
    let stats = unsafe { (straddling as *const ProcessStats).read_unaligned() };
    assert!(stats.wall_ns > 0 && stats.pid != 0, "process_stats wrote a snapshot with no wall time or pid in it");

    unsafe { syscall::vm_release(region, 2 * PAGE as usize) }.expect("munmap");
    println!("a typed syscall argument across a page boundary is read and written whole");
}
