//! Loom: the durability generations under a racing writer.
//!
//! The subject is `kernel/src/durability.rs` compiled into this crate
//! unshimmed, driven in the exact call order `Vfs::flush_file` and
//! `Vfs::sync_mount` use: snapshot-with-copy under the lock, device work with
//! the lock dropped, settle with the snapshot. What the models prove is the
//! accounting — no interleaving of one writer against one flusher ends with
//! debt discharged over bytes the device does not hold. What they do not prove
//! is that the kernel presents the snapshot only on its success paths; the
//! type makes any other discharge unwritable, and the guest controls
//! (`redirty_mid_flush`, `fsync_failed_commit`) hold the end-to-end claim.
//!
//! The negative control is a cargo feature rather than a comment:
//!
//! ```text
//! cargo test --manifest-path kernel-loom/Cargo.toml --features durability-settle-blind \
//!   --test durability
//! ```
//!
//! turns `settle` back into the pre-generation blind clear, and both
//! two-threaded models here must red — a write landing between the copy and
//! the settle is marked delivered without having been copied.

#![cfg(feature = "loom")]

use kernel_loom::durability::Owed;
use loom::sync::{Arc, Mutex};

/// One cached page and its debt; the u32 stands for the page's bytes.
struct Page {
    bytes: u32,
    dirt: Owed,
}

/// `Vfs::flush_file`'s per-page protocol: copy and snapshot in one lock
/// acquisition, write the copy to the device outside it, settle with what was
/// copied.
fn flush(page: &Mutex<Page>, device: &Mutex<u32>) {
    let (copied, upto) = {
        let page = page.lock().unwrap();
        (page.bytes, page.dirt.snapshot())
    };
    *device.lock().unwrap() = copied;
    page.lock().unwrap().dirt.settle(upto);
}

/// A write racing one flush either reaches the device or stays owed — and the
/// retry the kernel believes in (`fsync`'s next attempt) then delivers it.
#[test]
fn a_page_marked_clean_is_on_the_device() {
    loom::model(|| {
        let page = Arc::new(Mutex::new(Page { bytes: 0, dirt: Owed::new() }));
        let device = Arc::new(Mutex::new(0u32));

        let writer = {
            let page = page.clone();
            loom::thread::spawn(move || {
                let mut page = page.lock().unwrap();
                page.bytes = 7;
                page.dirt.record_write();
            })
        };
        flush(&page, &device);
        writer.join().unwrap();

        if page.lock().unwrap().dirt.is_owed() {
            flush(&page, &device);
        }
        let page = page.lock().unwrap();
        assert!(
            !page.dirt.is_owed() && *device.lock().unwrap() == page.bytes,
            "a page marked clean is not on the device: the settle covered a write the flush \
             never copied",
        );
    });
}

/// The mount's commit debt: a settle covers only writes the device flush could
/// have committed — one recorded after the snapshot stays owed, so the next
/// `fsync` still reaches the device (F5's second-call lie is unwritable).
#[test]
fn a_settled_commit_covers_only_flushed_writes() {
    loom::model(|| {
        // (writes in the device's cache, the mount's debt, writes made durable).
        let mount = Arc::new(Mutex::new((0u32, Owed::new(), 0u32)));

        let writer = {
            let mount = mount.clone();
            loom::thread::spawn(move || {
                let mut m = mount.lock().unwrap();
                m.0 += 1;
                m.1.record_write();
            })
        };
        let upto = mount.lock().unwrap().1.snapshot();
        {
            // The device flush commits whatever its cache holds at this instant.
            let mut m = mount.lock().unwrap();
            m.2 = m.0;
        }
        mount.lock().unwrap().1.settle(upto);
        writer.join().unwrap();

        let m = mount.lock().unwrap();
        assert!(
            m.1.is_owed() || m.2 == m.0,
            "commit debt settled while a write had not been made durable",
        );
    });
}

/// A flush that failed presents nothing, so the debt stands and the next
/// caller still owes the device — the discharge is the settlement, not the attempt.
#[test]
fn a_failed_commit_settles_nothing() {
    loom::model(|| {
        let mut debt = Owed::new();
        debt.record_write();
        let _upto = debt.snapshot();
        assert!(debt.is_owed(), "an unpresented settlement discharged the debt");
    });
}
