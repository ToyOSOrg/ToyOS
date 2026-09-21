//! Loom: Ctrl+Alt+D's request word, a pass on each side of it.
//!
//! A thread here is a scheduler pass: one that may serve takes the request and
//! reports until a report ends with nothing filed during it, as
//! `sched::dump::serve` does; one that may not leaves it. The report's own state
//! is a cell, so two reports at once are loom's `Concurrent write accesses`.
#![cfg(feature = "loom")]

use kernel_loom::dump_request::{DumpRequest, Left};
use loom::cell::UnsafeCell;
use loom::sync::Arc;

struct Machine {
    request: DumpRequest,
    reports: UnsafeCell<u32>,
}

impl Machine {
    fn new() -> Arc<Self> {
        Arc::new(Self { request: DumpRequest::new(), reports: UnsafeCell::new(0) })
    }

    fn report(&self) {
        self.reports.with_mut(|reports| {
            // SAFETY: the word's reporting bit is what makes this exclusive, and loom is the judge of that.
            unsafe { *reports += 1 }
        });
    }

    /// `sched::dump::serve`, without the read a pass with nothing pending pays instead.
    fn serve(&self) {
        if !self.request.take() {
            return;
        }
        loop {
            self.report();
            if !self.request.end_report() {
                return;
            }
        }
    }

    fn reports(&self) -> u32 {
        self.reports.with(|reports| {
            // SAFETY: every other thread has been joined.
            unsafe { *reports }
        })
    }
}

/// The key pressed on one CPU while another reports: the second request gets a
/// report of its own, from the filing CPU's pass or from the first report's end,
/// and nothing is left pending with nobody to take it.
#[test]
fn a_request_filed_during_a_report_is_reported() {
    loom::model(|| {
        let machine = Machine::new();
        machine.request.file();
        assert!(machine.request.take(), "a pass that may serve did not take a pending request");

        let filer = {
            let machine = machine.clone();
            loom::thread::spawn(move || {
                machine.request.file();
                machine.serve();
            })
        };

        loop {
            machine.report();
            if !machine.request.end_report() {
                break;
            }
        }
        filer.join().unwrap();

        assert!(!machine.request.pending(), "a request is pending and no pass is obliged to take it");
        assert_eq!(machine.reports(), 2, "two requests, the second filed after the first was taken");
    });
}

/// Two passes that may serve, one request: one report.
#[test]
fn one_request_is_taken_once() {
    loom::model(|| {
        let machine = Machine::new();
        machine.request.file();

        let other = {
            let machine = machine.clone();
            loom::thread::spawn(move || machine.serve())
        };
        machine.serve();
        other.join().unwrap();

        assert!(!machine.request.pending(), "the request outlived both passes");
        assert_eq!(machine.reports(), 1, "one request was reported other than once");
    });
}

/// Two passes that may not serve and one that may: the request is announced at
/// most once, and by nobody once it has been taken unannounced.
#[test]
fn a_request_is_announced_at_most_once() {
    loom::model(|| {
        let machine = Machine::new();
        machine.request.file();

        let leaver = {
            let machine = machine.clone();
            loom::thread::spawn(move || machine.request.leave())
        };
        let server = {
            let machine = machine.clone();
            loom::thread::spawn(move || machine.serve())
        };
        let mine = machine.request.leave();
        let theirs = leaver.join().unwrap();
        server.join().unwrap();

        assert!(
            !(mine == Left::First && theirs == Left::First),
            "two passes each said they were the first to leave one request",
        );
        assert_eq!(machine.reports(), 1, "leaving a request cost it its report");
    });
}

/// A pass leaves a request while a report runs: the announcement survives in the
/// word beside the reporting bit, and the report's end still takes the request.
#[test]
fn a_request_left_during_a_report_is_still_taken_by_its_end() {
    loom::model(|| {
        let machine = Machine::new();
        machine.request.file();
        assert!(machine.request.take());

        let leaver = {
            let machine = machine.clone();
            loom::thread::spawn(move || {
                machine.request.file();
                let left = machine.request.leave();
                assert_ne!(left, Left::Nothing, "a pass did not see the request it filed itself");
            })
        };
        leaver.join().unwrap();

        machine.report();
        assert!(machine.request.end_report(), "the report's end did not take the request filed during it");
        machine.report();
        assert!(!machine.request.end_report());
        assert_eq!(machine.reports(), 2);
    });
}
