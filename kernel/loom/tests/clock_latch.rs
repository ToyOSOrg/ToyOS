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
