//! Every CPU's deep idle periods across a span of an idle machine, once
//! `/system/bin/acpiserver` has handed the kernel its processors' idle
//! states, printed for the `idle_entered` metal row to judge
//! (`tests/toyos.rs`). It asserts nothing: whether a CPU chose a declared
//! state is the kernel's count to say.
//!
//! **A period is counted once the CPU is back from it**, after the
//! interrupt that ended it was handled, and the read that ends the span is
//! itself a kick to every CPU: a CPU that waited deep through the whole span
//! is counted only by a later round. So the span ends with a read that wakes
//! every CPU, and the periods are those a fresh round after it counts.

use std::time::{Duration, Instant};

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::syscap::SysCap;
use toyos_abi::counters::{Counter, RawRecord, Record};
use toyos_abi::syscall;
use toyos_logstream::program_line;

#[path = "../served_log.rs"]
mod served_log;

/// The idle span: a thousand residencies of a 1 ms state.
const IDLE: Duration = Duration::from_secs(1);

fn round(cap: &SysCap) -> Vec<Record> {
    let mut raw = vec![RawRecord::EMPTY; syscall::cpu_count() as usize];
    let n = cap.counters(&mut raw).expect("the estate's capability reads the counters");
    raw[..n].iter().map(|r| Record::decode(r).expect("a record that decodes")).collect()
}

fn main() {
    let cap: SysCap = Endowments::get().take(SYSCAP_LABEL).expect("test-runner endows a capability");
    served_log::Log::open().until("the ACPI server's idle states", served_log::SERVER_START, |line| {
        program_line(line).is_some_and(|said| said.tag == "acpiserver" && said.text.starts_with(acpiserver_api::IDLE_CENSUS))
    });
    let before = round(&cap);
    let began = Instant::now();
    std::thread::sleep(IDLE);
    round(&cap);
    let span = began.elapsed();
    // A round issued after this call began, at least `INTERVAL_NS` after
    // the waking round's (`kernel::counters`): every period that round ended is
    // in it.
    let after = round(&cap);
    for (b, a) in before.iter().zip(&after) {
        let periods = a.get(Counter::DeepIdles).zip(b.get(Counter::DeepIdles)).map(|(a, b)| a - b);
        let Some(periods) = periods else { panic!("cpu{} carries no deep idle count: {b:?} then {a:?}", a.cpu) };
        println!(
            "idle_entered: cpu{} {periods} deep idle periods in {} ns{}",
            a.cpu,
            span.as_nanos(),
            if a.stale || b.stale { ", stale" } else { "" },
        );
    }
}
