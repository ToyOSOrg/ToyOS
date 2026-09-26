//! A program that has the kernel write three batches of records and then says
//! one line, on a boot whose `logd` reads no record until that line
//! (`tests/logholdcase`'s `--hold-ring-until`): the line is read while every
//! one of those records is unread, and must land in `/log` after them all.
//! `log_program_line_after_its_records` runs it.

/// Three times what `logd` asks of the ring at once (`BATCH`, 64).
const RECORDS: usize = 192;

/// A retired syscall's number: each call is refused and is one kernel record
/// naming it (`kernel/src/syscall/dispatch.rs`'s `retired_syscall`).
const RETIRED: u64 = 26;

fn main() {
    for _ in 0..RECORDS {
        let ret: u64;
        // SAFETY: a register-only `syscall` whose number the kernel refuses
        // without reading any argument; nothing in this process is touched.
        unsafe {
            core::arch::asm!(
                "syscall",
                in("rdi") RETIRED,
                lateout("rax") ret,
                out("rcx") _,
                out("r11") _,
            );
        }
        assert_ne!(ret, 0, "syscall {RETIRED} answered as if it were live");
    }
    println!("log hold: said after {RECORDS} records");
}
