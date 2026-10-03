//! Loom: a panic console reader keeps one publication whole or none.
//!
//! The descriptor is four words a painter draws through; half of one
//! publication and half of the next is a pointer with another mode's stride.
//! One publisher replaces it (the boot, a mode set's window) while a painter
//! on another CPU snapshots it with no lock. The negative control is the
//! `seqlock-writer-fence-off` feature, which must red this file.
#![cfg(feature = "loom")]

use kernel_loom::panic_console_published::{Published, WORDS};
use loom::sync::Arc;
use loom::thread;

#[test]
fn a_snapshot_is_one_publication_whole() {
    loom::model(|| {
        let fb = Arc::new(Published::new());
        fb.publish([1; WORDS]);

        let publisher = {
            let fb = fb.clone();
            thread::spawn(move || fb.publish([2; WORDS]))
        };
        if let Some(words) = fb.snapshot() {
            assert!(
                words == [1; WORDS] || words == [2; WORDS],
                "a snapshot kept two publications' words: {words:?}"
            );
        }
        publisher.join().unwrap();

        // Quiescent: the second publication, whole.
        assert_eq!(fb.snapshot(), Some([2; WORDS]));
    });
}
