//! What timing protection costs, in nanoseconds a call: the switch itself and
//! `toyos-crypto`'s operations taking their own protection against the same
//! operations inside one their caller holds.
//!
//! **No threshold, and the host reads the numbers instead**, as
//! `syscall_cost`'s: a bound measured under TCG is meaningless, and the
//! numbers are for the owner's cost question. Each is the minimum over
//! [`REPS`] repetitions. The `timing_protection_cost` metal row runs it.
//!
//! On x86-64, an outer protection is two `SYS_TIMING_PROTECTION` calls, each
//! reading `IA32_UARCH_MISC_CTL` and writing it, so one write is about half
//! the pair less two queries; a switch between threads in different states
//! adds one such write and a compare.

use std::hint::black_box;
use std::time::Instant;

use toyos_abi::syscall::{self, TimingRequest};
use toyos_crypto::aead::SealingKey;
use toyos_crypto::{ed25519, x25519};

const REPS: usize = 9;

/// Nanoseconds a call of `op`, the least of [`REPS`] runs of `calls` calls.
fn ns(calls: u32, mut op: impl FnMut()) -> f64 {
    op();
    (0..REPS)
        .map(|_| {
            let begun = Instant::now();
            for _ in 0..calls {
                op();
            }
            begun.elapsed().as_nanos() as f64 / f64::from(calls)
        })
        .fold(f64::INFINITY, f64::min)
}

fn say(what: &str, ns: f64) {
    println!("timing_protection_cost: {what} {ns:.1} ns");
}

fn main() {
    let supported = syscall::timing_protection(TimingRequest::Query).is_ok();
    println!("timing_protection_cost: the machine {} timing protection", if supported { "has" } else { "has no" });

    let getpid = ns(20_000, || {
        black_box(syscall::getpid());
    });
    say("getpid", getpid);
    let query = ns(20_000, || {
        black_box(syscall::timing_protection(TimingRequest::Query).ok());
    });
    say("query", query);
    let pair = ns(20_000, || drop(black_box(toyos::timing::protect())));
    say("protect and end", pair);
    let nested = {
        let _outer = toyos::timing::protect();
        ns(200_000, || drop(black_box(toyos::timing::protect())))
    };
    say("protect and end inside a protection", nested);
    if supported && cfg!(target_arch = "x86_64") {
        say("one IA32_UARCH_MISC_CTL write, derived", (pair - 2.0 * query) / 2.0);
    }

    for len in [64, 1024, 16384] {
        let mut data = vec![0x70u8; len];
        let mut key = SealingKey::chacha20_poly1305([0x6b; 32], [0x24; 12]);
        let mut seal = || {
            black_box(key.seal(&[], &mut data).expect("far from the counter's end"));
        };
        compare(&format!("chacha20-poly1305 seal {len} B"), 2_000, &mut seal);
    }
    let signer = ed25519::SigningKey::from_seed([0x11; 32]);
    compare("ed25519 sign 64 B", 200, &mut || {
        black_box(signer.sign(&[0x22; 64]).expect("a short message"));
    });
    let peer = x25519::SecretKey::from_bytes([0x33; 32]).public_key();
    compare("x25519 agree", 200, &mut || {
        black_box(x25519::SecretKey::from_bytes([0x44; 32]).agree(&peer).expect("a peer key of full order"));
    });
}

/// `op` taking its own protection, and inside one held around it all.
fn compare(what: &str, calls: u32, op: &mut impl FnMut()) {
    let alone = ns(calls, &mut *op);
    let batched = {
        let _outer = toyos::timing::protect();
        ns(calls, &mut *op)
    };
    println!(
        "timing_protection_cost: {what}: {alone:.1} ns alone, {batched:.1} ns in a held protection, {:+.1}%",
        (alone / batched - 1.0) * 100.0,
    );
}
