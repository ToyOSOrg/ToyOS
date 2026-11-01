//! A typed syscall argument that crosses a page boundary is read and written
//! whole. The two pages of a mapping are two frames wherever the allocator put
//! them, so a value placed across the boundary is a copy cut at the seam: one
//! translated as a single run would read or write its tail in whatever frame
//! the allocator handed out after the first, and one refused would refuse a
//! value its own process may store.
//!
//! Two are reachable from userland: `fstat`'s `Stat`, a kernel write, and
//! `spawn`'s `SpawnArgs`, a kernel read whose `argv_ptr` and `argv_len` the
//! kernel then acts on.
//!
//! **The verdict is the bytes**: each straddling value is read back field by
//! field on both sides of the boundary, and a straddling `SpawnArgs` spawns.

#[path = "../image.rs"]
mod image;

use toyos_abi::RawHandle;
use toyos_abi::syscall::{
    self, VmAt, VmProt, OpenFlags, ProcessStats, SpawnArgs, Stat, SyscallError, SYS_FSTAT,
    SYS_PROCESS_STATS,
};

/// Where the child starts: `SpawnArgs` names a working directory or the spawn is refused.
const CWD: &str = "/";

const PAGE: u64 = 4096;
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

    let own = image::in_tmp("/system/bin/test_rs_abuse_page_straddle");
    let handle = syscall::open(own.as_bytes(), OpenFlags::READ).expect("open self");
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
    let argv = b"/system/bin/echo\0straddle\0";
    let echo = image::image_of("/system/bin/echo");
    let (image, image_len) = echo.spawn_words();
    unsafe {
        core::ptr::copy_nonoverlapping(argv.as_ptr(), base as *mut u8, argv.len());
    }
    let args = SpawnArgs {
        path_ptr: base,
        path_len: "/system/bin/echo".len() as u64,
        argv_ptr: base,
        argv_len: argv.len() as u64,
        env_ptr: 0,
        env_len: 0,
        endow_ptr: 0,
        endow_count: 0,
        labels_ptr: 0,
        labels_len: 0,
        cwd_ptr: CWD.as_ptr() as u64,
        cwd_len: CWD.len() as u64,
        image,
        image_len,
        place: u64::from(toyos_abi::HANDLE_INVALID.0),
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
