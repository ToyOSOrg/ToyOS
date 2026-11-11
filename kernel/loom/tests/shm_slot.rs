//! Loom: a region page's first touch, alone or as part of a fault's run.
//!
//! A region draws a page's frame at its first touch, and two CPUs may touch it
//! at once: two threads of a process, or two processes mapping the region. A
//! fault asks for its whole run at once. A touch takes the frames installed
//! with one load each; if any page it asks for is a hole, it takes the region's
//! first-touch lock, looks again, and draws the holes still left in one call.
//! What must hold, from every interleaving loom can build:
//!
//! * **One draw a page.** However the touches and runs interleave, each page
//!   draws one frame, against its own promise, and every toucher is handed it.
//! * **A frame comes with its zeros.** A toucher that takes a frame off the one
//!   load reads what its drawer wrote into it before installing it. On x86
//!   every store is a release and every load an acquire, so no guest test can
//!   see this fail.
//!
//! The negative case is a cargo feature:
//!
//! ```text
//! cargo test --manifest-path kernel/loom/Cargo.toml --features slot-install-relaxed \
//!   --test shm_slot
//! ```
//!
//! makes the install and the one load `Relaxed`, and
//! [`a_frame_is_handed_over_with_what_its_drawer_wrote`] must red.

#![cfg(feature = "loom")]

use kernel_loom::shm_slot::{frames, Slot};
use loom::cell::UnsafeCell;
use loom::sync::atomic::{AtomicUsize, Ordering};
use loom::sync::{Arc, Mutex};

/// Frames a touch may draw, by address: at most one a page of the region.
const PAGES: usize = 3;
const FRAME: u64 = 0x1000;

/// A region of [`PAGES`] pages: their slots, its first-touch lock, each frame's
/// contents, and how many frames were drawn and how many draws made them.
struct Region {
    slots: [Slot; PAGES],
    first_touch: Mutex<()>,
    contents: [UnsafeCell<u64>; PAGES],
    drawn: AtomicUsize,
    draws: AtomicUsize,
}

impl Region {
    fn new() -> Self {
        Self {
            slots: [Slot::hole(), Slot::hole(), Slot::hole()],
            first_touch: Mutex::new(()),
            contents: [UnsafeCell::new(0xa5), UnsafeCell::new(0xa5), UnsafeCell::new(0xa5)],
            drawn: AtomicUsize::new(0),
            draws: AtomicUsize::new(0),
        }
    }

    /// Touch pages `pages` as one run: each page's frame and what it holds.
    fn touch(&self, pages: &[usize]) -> Vec<(u64, u64)> {
        let slots: Vec<&Slot> = pages.iter().map(|&p| &self.slots[p]).collect();
        let mut out = vec![0u64; pages.len()];
        let mut drawn = vec![0u64; pages.len()];
        frames(
            &slots,
            &mut out,
            &mut drawn,
            || self.first_touch.lock().unwrap(),
            |fresh| {
                self.draws.fetch_add(1, Ordering::Relaxed);
                for frame in fresh.iter_mut() {
                    let k = self.drawn.fetch_add(1, Ordering::Relaxed);
                    assert!(k < PAGES, "more frames drawn than the region has pages");
                    // SAFETY: frame `k` is this draw's alone until it is installed.
                    self.contents[k].with_mut(|p| unsafe { *p = 0 });
                    *frame = FRAME * (k as u64 + 1);
                }
            },
        );
        out.iter()
            .map(|&frame| {
                let k = (frame / FRAME) as usize - 1;
                // SAFETY: a frame read after the slots handed it over; nothing
                // writes it after its drawer installed it.
                (frame, self.contents[k].with(|p| unsafe { *p }))
            })
            .collect()
    }
}

/// Two touches of one page draw one frame between them and are both handed it.
#[test]
fn two_touches_draw_once_and_are_handed_one_frame() {
    loom::model(|| {
        let region = Arc::new(Region::new());
        let other = {
            let region = region.clone();
            loom::thread::spawn(move || region.touch(&[1])[0].0)
        };
        let mine = region.touch(&[1])[0].0;
        let theirs = other.join().unwrap();
        assert_eq!(mine, theirs, "two touches of one page were handed two frames");
        assert_eq!(region.drawn.load(Ordering::Relaxed), 1, "a page drew more than once");
        assert_eq!(region.slots[1].drawn(), Some(mine), "the slot holds another frame than the touches were handed");
    });
}

/// Two runs that share a page draw each page of either once, in one draw a
/// run at most, and hand both the shared page's one frame.
#[test]
fn two_runs_draw_each_page_once() {
    loom::model(|| {
        let region = Arc::new(Region::new());
        let other = {
            let region = region.clone();
            loom::thread::spawn(move || region.touch(&[1, 2]))
        };
        let mine = region.touch(&[0, 1]);
        let theirs = other.join().unwrap();
        assert_eq!(mine[1].0, theirs[0].0, "two runs were handed two frames for the page they share");
        assert_eq!(region.drawn.load(Ordering::Relaxed), PAGES, "a page drew more than once, or not at all");
        assert!(region.draws.load(Ordering::Relaxed) <= 2, "a run drew its holes in more than one draw");
        let frames: Vec<u64> = region.slots.iter().map(|slot| slot.drawn().expect("a page touched and not drawn")).collect();
        assert_eq!(frames, [mine[0].0, mine[1].0, theirs[1].0], "the slots hold other frames than the runs were handed");
    });
}

/// A toucher handed a frame another drew reads the zeros its drawer wrote.
#[test]
fn a_frame_is_handed_over_with_what_its_drawer_wrote() {
    loom::model(|| {
        let region = Arc::new(Region::new());
        let other = {
            let region = region.clone();
            loom::thread::spawn(move || region.touch(&[0, 1]))
        };
        let mine = region.touch(&[1]);
        let theirs = other.join().unwrap();
        assert!(mine.iter().chain(&theirs).all(|&(_, holds)| holds == 0), "a toucher read a frame its drawer had not zeroed for it");
    });
}

/// A page nothing touched holds no frame; its first touch installs its draw,
/// and a later touch is handed that frame and draws none.
#[test]
fn a_touch_after_the_first_draws_nothing() {
    loom::model(|| {
        let region = Region::new();
        assert_eq!(region.slots[2].drawn(), None, "an untouched page holds a frame");
        let first = region.touch(&[2]);
        assert_eq!(region.touch(&[2]), first, "a later touch was handed another frame");
        assert_eq!(region.draws.load(Ordering::Relaxed), 1, "a later touch drew a frame");
    });
}
