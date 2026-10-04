//! Every CPU's counters across a span of an idle machine and then across every
//! CPU spinning, printed as `inspect kernel.*` renders them, for the `counters`
//! metal row to judge (`tests/toyos.rs`). It asserts nothing: frequency, busy
//! fraction and firmware interrupts are the hardware's to say.
//!
//! Three reads: `idle0` and `idle1` either side of [`IDLE`], and `spin` after
//! [`SPIN`] iterations on a thread per CPU begun at `idle1`. Each read's
//! records follow a line with the clock after it and what the read took, a
//! whole round each, none joining another's.

use std::time::{Duration, Instant};

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::syscap::SysCap;
use toyos_abi::counters::{RawRecord, Record};
use toyos_abi::syscall;

/// The idle span: long enough that a CPU's busy fraction is its idle one and
/// not the reads'.
const IDLE: Duration = Duration::from_secs(1);

/// Iterations of a dependent multiply-add per thread, about four cycles each:
/// over four seconds of a CPU at the T14's highest frequency, so `idle0` and
/// `spin` span more than twice its firmware's 2.2 s interrupt period whatever
/// the frequency.
const SPIN: u64 = 4_000_000_000;

fn read(cap: &SysCap, phase: &str) {
    let mut raw = vec![RawRecord::EMPTY; syscall::cpu_count() as usize];
    let asked = Instant::now();
    let n = cap.counters(&mut raw).expect("the estate's capability reads the counters");
    let took = asked.elapsed();
    println!("counters_metal {phase}: at {} ns, the read took {} ns", toyos_abi::clock::nanos_since_boot(), took.as_nanos());
    let records: Vec<Record> = raw[..n].iter().map(|r| Record::decode(r).expect("a record that decodes")).collect();
    for (path, value) in toyos_inspect::kernel::render(&records).expect("one record per cpu") {
        println!("counters_metal {phase}: {}", toyos_inspect::line(&path, &value));
    }
}

fn main() {
    let cap: SysCap = Endowments::get().take(SYSCAP_LABEL).expect("test-runner endows a capability");
    read(&cap, "idle0");
    std::thread::sleep(IDLE);
    read(&cap, "idle1");
    std::thread::scope(|s| {
        for _ in 0..syscall::cpu_count() {
            s.spawn(|| {
                let mut x = 1u64;
                for _ in 0..SPIN {
                    x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
                }
            });
        }
    });
    read(&cap, "spin");
}
