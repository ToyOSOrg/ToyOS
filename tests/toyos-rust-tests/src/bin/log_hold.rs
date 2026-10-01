//! A program that has the kernel write three batches of records and then says
//! one line, which must land in `/log` after them all: `logd` reads a
//! program's ring before the kernel's records, so only the stamps order them.
//! `log_program_line_after_its_records` runs it.

use toyos_abi::syscall::debug_action::LOG_PATTERNED;

/// Three times what `logd` asks of the ring at once (`BATCH`, 64).
const RECORDS: u64 = 192;

fn main() {
    for index in 0..RECORDS {
        let answer = toyos_abi::syscall::debug_with(LOG_PATTERNED, index);
        assert_eq!(answer, 0, "SYS_DEBUG LOG_PATTERNED answered {answer:#x} at index {index}");
    }
    println!("log hold: said after {RECORDS} records");
}
