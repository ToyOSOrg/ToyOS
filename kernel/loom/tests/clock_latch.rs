//! Loom: a reader of the clock page's latch gets one line whole, and never an
//! older one after a newer.
//!
//! The line is three words a reader turns into a time; half of one
//! publication and half of the next is a clock that steps. One writer
//! replaces it (the kernel's retune) while a reader on another CPU reads it
//! with no lock, and a reader never waits for the writer. The negative control
//! is the `clock-latch-fence-off` feature, which must red this file.
#![cfg(feature = "loom")]

use kernel_loom::clock_latch::Latch;
use loom::sync::Arc;
use loom::thread;

const WORDS: usize = 3;

/// A bound so the two publications' twelve stores finish exploring; under it
/// the `clock-latch-fence-off` control still reaches a torn read.
const PREEMPTIONS: usize = 3;

fn model(f: impl Fn() + Sync + Send + 'static) {
    let mut builder = loom::model::Builder::new();
    builder.preemption_bound = Some(PREEMPTIONS);
    builder.check(f);
}

/// Two publications, so the reader can meet either copy being rewritten.
#[test]
fn a_read_is_one_line_whole() {
    model(|| {
        let latch = Arc::new(Latch::<WORDS>::new());
        latch.publish([1; WORDS]);
        let writer = {
            let latch = latch.clone();
            thread::spawn(move || {
                latch.publish([2; WORDS]);
                latch.publish([3; WORDS]);
            })
        };
        let line = latch.read();
        assert!(line == [1; WORDS] || line == [2; WORDS] || line == [3; WORDS], "a read kept two lines' words: {line:?}");
        writer.join().unwrap();
        assert_eq!(latch.read(), [3; WORDS]);
    });
}

#[test]
fn a_later_read_is_never_an_older_line() {
    model(|| {
        let latch = Arc::new(Latch::<WORDS>::new());
        latch.publish([1; WORDS]);
        let writer = {
            let latch = latch.clone();
            thread::spawn(move || latch.publish([2; WORDS]))
        };
        let first = latch.read();
        let second = latch.read();
        assert!(second[0] >= first[0], "a later read went back to an older line: {first:?} then {second:?}");
        writer.join().unwrap();
    });
}

/// The counter a reader turns a line into a time with, modelled as memory the
/// writer advances only once the line that starts there is published.
fn superseded(read: impl Fn(&Latch<WORDS>, &loom::sync::atomic::AtomicU64) -> ([u64; WORDS], u64) + Sync + Send + 'static) {
    use loom::sync::atomic::{AtomicU64, Ordering};
    model(move || {
        let latch = Arc::new(Latch::<WORDS>::new());
        let counter = Arc::new(AtomicU64::new(0));
        latch.publish([1; WORDS]);
        let writer = {
            let (latch, counter) = (latch.clone(), counter.clone());
            thread::spawn(move || {
                latch.publish([2; WORDS]);
                counter.store(2, Ordering::Release);
            })
        };
        let (line, now) = read(&latch, &counter);
        assert!(now < 2 || line == [2; WORDS], "a reading at {now} extrapolated the superseded line {line:?}");
        writer.join().unwrap();
    });
}

/// A counter read inside the section, after the line's words, is read again
/// with the line when a publication lands between them.
#[test]
fn a_counter_read_inside_the_section_never_extrapolates_a_superseded_line() {
    superseded(|latch, counter| latch.read_with(|| counter.load(loom::sync::atomic::Ordering::Acquire)));
}

/// What the clock read before: the counter after the section, which a reader
/// preempted between the two pairs with a line replaced meanwhile.
#[test]
#[should_panic(expected = "extrapolated the superseded line")]
fn a_counter_read_after_the_section_can_extrapolate_a_superseded_line() {
    superseded(|latch, counter| (latch.read(), counter.load(loom::sync::atomic::Ordering::Acquire)));
}
