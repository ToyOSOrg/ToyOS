//! Loom's own exploration, which every model in this tree rests on: a write is
//! run ahead of another thread's earlier load even when the writing thread
//! loaded the word last.

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
