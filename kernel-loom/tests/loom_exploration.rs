//! Loom's own exploration, which every model in this tree rests on: a write is
//! run ahead of another thread's earlier load even when the writing thread
//! loaded the word last.
//!
//! Loom 0.7.2 as published races a store or read-modify-write only against the
//! word's single last access, so it runs one schedule of each probe below, and
//! a model whose defect needs the other schedule passes. The root
//! `[patch.crates-io]` loom fork races it against every thread's last load
//! (`forks.toml`), and these probes red if that is lost. Shuttle's depth-first
//! search, which shares no code with loom, is the oracle for what each probe
//! must reach.

#![cfg(feature = "loom")]

use std::collections::BTreeSet;
use std::sync::atomic::Ordering::SeqCst;
use std::sync::{Arc, Mutex};

/// What the model thread's load may return when the writer loads the word and
/// then stores what it read plus one.
const AFTER_A_STORE: [usize; 2] = [0, 1];

/// The same when the writer loads the word and then adds one to it twice.
const AFTER_TWO_ADDS: [usize; 3] = [0, 1, 2];

#[test]
fn loom_runs_a_store_ahead_of_an_earlier_load() {
    let seen = Arc::new(Mutex::new(BTreeSet::new()));
    let record = seen.clone();
    loom::model(move || {
        let word = loom::sync::Arc::new(loom::sync::atomic::AtomicUsize::new(0));
        let writer = {
            let word = word.clone();
            loom::thread::spawn(move || {
                let v = word.load(SeqCst);
                word.store(v + 1, SeqCst);
            })
        };
        record.lock().unwrap().insert(word.load(SeqCst));
        writer.join().unwrap();
    });
    assert_eq!(
        *seen.lock().unwrap(),
        BTreeSet::from(AFTER_A_STORE),
        "loom never ran the store ahead of the model thread's load: the loom fork is lost",
    );
}

#[test]
fn loom_runs_a_read_modify_write_ahead_of_an_earlier_load() {
    let seen = Arc::new(Mutex::new(BTreeSet::new()));
    let record = seen.clone();
    loom::model(move || {
        let word = loom::sync::Arc::new(loom::sync::atomic::AtomicUsize::new(0));
        let writer = {
            let word = word.clone();
            loom::thread::spawn(move || {
                word.load(SeqCst);
                word.fetch_add(1, SeqCst);
                word.fetch_add(1, SeqCst);
            })
        };
        record.lock().unwrap().insert(word.load(SeqCst));
        writer.join().unwrap();
    });
    assert_eq!(
        *seen.lock().unwrap(),
        BTreeSet::from(AFTER_TWO_ADDS),
        "loom never ran an add ahead of the model thread's load: the loom fork is lost",
    );
}

#[test]
fn shuttle_reaches_exactly_what_the_loom_probes_demand() {
    let after_a_store = Arc::new(Mutex::new(BTreeSet::new()));
    let record = after_a_store.clone();
    shuttle::check_dfs(
        move || {
            let word = shuttle::sync::Arc::new(shuttle::sync::atomic::AtomicUsize::new(0));
            let writer = {
                let word = word.clone();
                shuttle::thread::spawn(move || {
                    let v = word.load(SeqCst);
                    word.store(v + 1, SeqCst);
                })
            };
            record.lock().unwrap().insert(word.load(SeqCst));
            writer.join().unwrap();
        },
        None,
    );

    let after_two_adds = Arc::new(Mutex::new(BTreeSet::new()));
    let record = after_two_adds.clone();
    shuttle::check_dfs(
        move || {
            let word = shuttle::sync::Arc::new(shuttle::sync::atomic::AtomicUsize::new(0));
            let writer = {
                let word = word.clone();
                shuttle::thread::spawn(move || {
                    word.load(SeqCst);
                    word.fetch_add(1, SeqCst);
                    word.fetch_add(1, SeqCst);
                })
            };
            record.lock().unwrap().insert(word.load(SeqCst));
            writer.join().unwrap();
        },
        None,
    );

    assert_eq!(*after_a_store.lock().unwrap(), BTreeSet::from(AFTER_A_STORE));
    assert_eq!(*after_two_adds.lock().unwrap(), BTreeSet::from(AFTER_TWO_ADDS));
}
