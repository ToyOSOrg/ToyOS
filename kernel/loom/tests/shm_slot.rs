//! Loom: a region page's first touch.
//!
//! A region draws a page's frame at its first touch, and two CPUs may touch it
//! at once: two threads of a process, or two processes mapping the region.
//! Each draws a zeroed frame and races to install it. What must hold, from
//! every interleaving loom can build:
//!
//! * **One frame a page.** Both touchers are handed the same frame, and every
//!   draw but the installed one goes back exactly once, so no frame is lost or
//!   mapped twice. A touch that finds the frame installed draws none.
//! * **A frame comes with its zeros.** A toucher handed another's frame reads
//!   what its drawer wrote into it before installing it. On x86 every store is
//!   a release and every load an acquire, so no guest test can see this fail.
//!
//! The negative case is a cargo feature:
//!
//! ```text
//! cargo test --manifest-path kernel/loom/Cargo.toml --features slot-install-relaxed \
//!   --test shm_slot
//! ```
//!
//! makes the install and its loads `Relaxed`, and
//! [`a_frame_is_handed_over_with_what_its_drawer_wrote`] must red.

#![cfg(feature = "loom")]

use kernel_loom::shm_slot::Slot;
use loom::cell::UnsafeCell;
use loom::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use loom::sync::Arc;

/// Two frames, by the address each touch draws.
const FRAMES: [u64; 2] = [0x1000, 0x2000];

/// A page two touches race for: its slot, each frame's contents, how many
/// frames were drawn and what was given back.
struct Page {
    slot: Slot,
    contents: [UnsafeCell<u64>; 2],
    draws: AtomicUsize,
    given_back: AtomicUsize,
    returned: AtomicU64,
}

impl Page {
    fn new() -> Self {
        Self {
            slot: Slot::hole(),
            contents: [UnsafeCell::new(0xa5), UnsafeCell::new(0xa5)],
            draws: AtomicUsize::new(0),
            given_back: AtomicUsize::new(0),
            returned: AtomicU64::new(0),
        }
    }

    /// Touch `i`: draw frame `i`, zero it, and race to install it; the frame
    /// handed back and what it holds.
    fn touch(&self, i: usize) -> (u64, u64) {
        let frame = self.slot.frame(
            || {
                self.draws.fetch_add(1, Ordering::Relaxed);
                // SAFETY: frame `i` is this touch's alone until it is installed.
                self.contents[i].with_mut(|p| unsafe { *p = 0 });
                FRAMES[i]
            },
            |lost| {
                self.given_back.fetch_add(1, Ordering::Relaxed);
                self.returned.store(lost, Ordering::Relaxed);
            },
        );
        let at = FRAMES.iter().position(|&f| f == frame).expect("a frame no touch drew");
        // SAFETY: a frame read after the slot handed it over; nothing writes it
        // after its drawer installed it.
        (frame, self.contents[at].with(|p| unsafe { *p }))
    }
}

/// Two touches are handed one frame, and a draw that lost goes back once.
#[test]
fn two_touches_keep_one_frame_and_give_the_other_back() {
    loom::model(|| {
        let page = Arc::new(Page::new());
        let other = {
            let page = page.clone();
            loom::thread::spawn(move || page.touch(1).0)
        };
        let mine = page.touch(0).0;
        let theirs = other.join().unwrap();
        assert_eq!(mine, theirs, "two touches of one page were handed two frames");
        let (draws, back) = (page.draws.load(Ordering::Relaxed), page.given_back.load(Ordering::Relaxed));
        assert_eq!(back, draws - 1, "{draws} frames drawn and {back} given back: every draw but the installed one goes back");
        if back == 1 {
            assert_ne!(page.returned.load(Ordering::Relaxed), mine, "the frame handed out went back");
        }
        assert_eq!(page.slot.drawn(), Some(mine), "the slot holds another frame than the touches were handed");
    });
}

/// A toucher handed the other's frame reads the zeros its drawer wrote.
#[test]
fn a_frame_is_handed_over_with_what_its_drawer_wrote() {
    loom::model(|| {
        let page = Arc::new(Page::new());
        let other = {
            let page = page.clone();
            loom::thread::spawn(move || page.touch(1).1)
        };
        let mine = page.touch(0).1;
        let theirs = other.join().unwrap();
        assert_eq!((mine, theirs), (0, 0), "a toucher read a frame its drawer had not zeroed for it");
    });
}

/// A page nothing touched holds no frame; its first touch installs its draw,
/// and a later touch is handed that frame and draws none.
#[test]
fn a_touch_after_the_first_draws_nothing() {
    loom::model(|| {
        let page = Page::new();
        assert_eq!(page.slot.drawn(), None, "an untouched page holds a frame");
        assert_eq!(page.touch(1), (FRAMES[1], 0));
        assert_eq!(page.touch(0), (FRAMES[1], 0), "a later touch was handed another frame");
        assert_eq!(page.draws.load(Ordering::Relaxed), 1, "a later touch drew a frame");
    });
}
