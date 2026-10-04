//! Scout, never landed: what cpu7 does across an idle second on the T14.
//!
//! The `counters` row prints its `idle0` read, 81 lines, at the start of the
//! very second it then measures, and every boot of it has put the `/log`
//! fileserver on cpu7. This binary measures back-to-back idle seconds, each
//! between two counters rounds, under one of three arms:
//!
//! - `quiet`: nothing printed in the second (the negative control);
//! - `loud`: the second's opening read printed exactly as the row prints it;
//! - `dose`: that print [`DOSE`] times over.
//!
//! Nothing else is printed until every second is measured, so no arm's lines
//! reach the log inside another arm's second. A warm-up `loud` second goes
//! first, because it carries the boot's first stick flush; the rounds then
//! rotate the arms' order, so a second's spill into the next is read as such.

use std::time::Duration;

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::syscap::SysCap;
use toyos_abi::counters::{Counter, RawRecord, Record};
use toyos_abi::syscall;

const IDLE: Duration = Duration::from_secs(1);
const DOSE: usize = 4;
const ROUNDS: usize = 4;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Arm {
    Quiet,
    Loud,
    Dose,
}

fn read(cap: &SysCap) -> (u64, Vec<Record>) {
    let mut raw = vec![RawRecord::EMPTY; syscall::cpu_count() as usize];
    let n = cap.counters(&mut raw).expect("the estate's capability reads the counters");
    let at = toyos_abi::clock::nanos_since_boot();
    (at, raw[..n].iter().map(|r| Record::decode(r).expect("a record that decodes")).collect())
}

/// What the row prints for one read, line for line.
fn say(at: u64, records: &[Record], times: usize) {
    for _ in 0..times {
        println!("cpu7scout_echo idle0: at {at} ns, the read took 0 ns");
        for (path, value) in toyos_inspect::kernel::render(records).expect("one record per cpu") {
            println!("cpu7scout_echo idle0: {}", toyos_inspect::line(&path, &value));
        }
    }
}

fn delta(a: &Record, b: &Record, c: Counter) -> u64 {
    b.get(c).expect("carried") - a.get(c).expect("carried")
}

fn main() {
    let cap: SysCap = Endowments::get().take(SYSCAP_LABEL).expect("test-runner endows a capability");
    let order = [Arm::Quiet, Arm::Loud, Arm::Dose];
    let mut plan = vec![("warmup", Arm::Loud)];
    for round in 0..ROUNDS {
        for k in 0..order.len() {
            plan.push(("round", order[(round + k) % order.len()]));
        }
    }
    let mut seconds = Vec::new();
    let mut before = read(&cap);
    for &(kind, arm) in &plan {
        match arm {
            Arm::Quiet => {}
            Arm::Loud => say(before.0, &before.1, 1),
            Arm::Dose => say(before.0, &before.1, DOSE),
        }
        std::thread::sleep(IDLE);
        let after = read(&cap);
        seconds.push((kind, arm, before, after.clone()));
        before = after;
    }
    for (i, (kind, arm, (t0, r0), (t1, r1))) in seconds.iter().enumerate() {
        assert!(r0.iter().chain(r1).all(|r| !r.stale), "second {i}: a cpu answered stale");
        let busy: Vec<String> = r0
            .iter()
            .zip(r1)
            .map(|(a, b)| format!("{}", delta(a, b, Counter::Mperf) * 1_000_000 / delta(a, b, Counter::Stamp)))
            .collect();
        let kicks: Vec<String> = r0.iter().zip(r1).map(|(a, b)| format!("{}", delta(a, b, Counter::Kicks))).collect();
        let smi = delta(&r0[0], &r1[0], Counter::Smi);
        println!(
            "cpu7_scout second {i} {kind} {arm:?}: from {t0} ns to {t1} ns, smi +{smi}, busy ppm [{}], kicks [{}]",
            busy.join(" "),
            kicks.join(" ")
        );
    }
}
