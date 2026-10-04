//! `/system/bin/trace` — the kernel's diary, as it stood when asked.
//!
//! Every record the CPUs' rings still hold, oldest first and merged across
//! CPUs, one line each as `toyos-trace` prints it, up to the moment the
//! program started; on stderr, how many records were overwritten before the
//! read reached them. The diary is read on this program's `SysCap`, whose
//! `trace` right its manifest row grants.
//!
//! Exit 2 is a refusal, named on stderr: no capability, a capability without
//! `trace`, and a record that does not decode.

use std::io::Write;

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::syscap::SysCap;
use toyos_abi::clock;
use toyos_abi::syscall::SyscallError;
use toyos_abi::trace::{TraceCursor, TraceRecord};
use toyos_trace::Entry;

const USAGE: &str = "usage: trace";

/// Records asked for at once; above any machine's CPU count, which a read needs room for.
const BATCH: usize = 1024;

fn main() {
    if std::env::args().len() > 1 {
        refuse(USAGE);
    }
    let Some(cap) = Endowments::get().take::<SysCap>(SYSCAP_LABEL) else {
        refuse("trace: this program holds no system capability, so the kernel's diary is not its to read");
    };
    let page = clock::page();
    let until = clock::nanos_since_boot();

    let mut cursor = TraceCursor::new();
    let mut raw = vec![TraceRecord::EMPTY; BATCH];
    let mut lost = 0;
    let mut stdout = std::io::stdout().lock();
    'read: loop {
        let n = match cap.trace(&mut cursor, &mut raw) {
            Ok(n) => n,
            Err(SyscallError::PermissionDenied) => {
                refuse("trace: the kernel refused: this program's capability does not carry `trace`")
            }
            Err(e) => refuse(&format!("trace: the diary would not read: {e:?}")),
        };
        lost += cursor.lost();
        for record in &raw[..n] {
            let entry = Entry::decode(record).unwrap_or_else(|why| refuse(&format!("trace: {record:?}: {why}")));
            if clock::nanos_between(page.counter_at_boot, page.period_fs, entry.stamp) > until {
                break 'read;
            }
            // A reader whose pipe closed early (`| head`) has nobody left to tell.
            if writeln!(stdout, "{}", entry.line(page)).is_err() {
                return;
            }
        }
        if n < raw.len() {
            break;
        }
    }
    if stdout.flush().is_err() {
        return;
    }
    if lost > 0 {
        eprintln!("trace: {lost} records were overwritten before this read reached them");
    }
}

fn refuse(why: &str) -> ! {
    eprintln!("{why}");
    std::process::exit(2);
}
