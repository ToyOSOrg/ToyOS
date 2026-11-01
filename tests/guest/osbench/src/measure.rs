//! The two shapes most benchmarks here take: an operation timed alone many
//! times, and a transfer timed whole a few times.

use std::time::Instant;

use crate::report::{refuse, result};
use crate::sys::{self, Counts};
use crate::ns_since;

/// Samples and the fields printed beside them.
pub type Samples<'a> = (Vec<u64>, Vec<(&'a str, String)>);

/// `op(i)` `warmup` times unread, then `samples` times each timed alone: ns
/// per operation, with the process's own accounting per operation beside it.
/// An operation that fails refuses the benchmark with its error.
pub fn each(
    bench: &str,
    warmup: usize,
    samples: usize,
    extra: &[(&str, String)],
    mut op: impl FnMut(usize) -> Result<(), String>,
) {
    let mut run = || -> Result<Samples<'_>, String> {
        let ran = crate::ran::start();
        for i in 0..warmup {
            op(i)?;
        }
        crate::clock::settle();
        let before = sys::counts();
        let mut ns = Vec::with_capacity(samples);
        for i in 0..samples {
            let t = Instant::now();
            op(warmup + i)?;
            ns.push(ns_since(t));
        }
        let mut counts = Counts::per_op(&before, samples as u64);
        counts.extend(ran.fields());
        Ok((ns, counts))
    };
    match run() {
        Ok((mut ns, counts)) => {
            let fields: Vec<(&str, String)> = extra.iter().cloned().chain(counts).collect();
            result(bench, "ns", &mut ns, &fields);
        }
        Err(why) => refuse(bench, why),
    }
}

/// `round()` once unread and `rounds` times timed whole, each moving `bytes`:
/// MB/s per round.
pub fn throughput(
    bench: &str,
    rounds: usize,
    bytes: u64,
    extra: &[(&str, String)],
    mut round: impl FnMut() -> Result<(), String>,
) {
    let mut run = || -> Result<Vec<u64>, String> {
        round()?;
        crate::clock::settle();
        (0..rounds)
            .map(|_| {
                let t = Instant::now();
                round()?;
                Ok(bytes * 1000 / ns_since(t).max(1))
            })
            .collect()
    };
    match run() {
        Ok(mut mbps) => result(bench, "MB/s", &mut mbps, extra),
        Err(why) => refuse(bench, why),
    }
}

/// An `io::Result` as this module's error.
pub fn io<T>(what: &str, r: std::io::Result<T>) -> Result<T, String> {
    r.map_err(|e| format!("{what}: {e}"))
}
