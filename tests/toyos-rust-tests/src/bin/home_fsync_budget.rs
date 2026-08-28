//! An fsync on `/home` whose first attempt is budget-refused (`fsync-budget-spent`)
//! must retry on a fresh budget and succeed — a `BudgetExpired` reaching the
//! bcachefs adapter as `Io` ends the syscall on attempt 1 instead (F9).
//! `tests/common/storage.rs::home_budget_refusal_retried` boots and judges this.

use std::fs::File;
use std::io::{Read, Write};

/// Mirrored in `tests/common/storage.rs::home_budget_refusal_retried`.
const PATH: &str = "/home/f9-budget.bin";
const LEN: usize = 3 * 4096 + 41;

fn pattern() -> Vec<u8> {
    (0..LEN).map(|i| (i.wrapping_mul(151) ^ 0x3C) as u8).collect()
}

fn main() {
    let want = pattern();
    let mut f = File::create(PATH).expect("create on /home");
    f.write_all(&want).expect("write");
    f.sync_all().expect(
        "fsync refused: a budget-expired first attempt must be retried on a fresh budget, \
         never returned as the device's word",
    );

    let mut got = Vec::new();
    File::open(PATH).expect("re-open").read_to_end(&mut got).expect("read back");
    assert_eq!(got, want, "the bytes changed across the refused-then-retried fsync");
    println!("budget-refused fsync retried to durable: {LEN} bytes on /home");
}
