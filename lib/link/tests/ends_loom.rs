//! A full ring's want on two CPUs, under loom: the producer that finds no room
//! and the consumer that releases one never both miss each other, so the
//! producer is never left asleep on room it has.
//!
//!   cargo test -p toyos-link --features mutate-want-relaxed --test ends_loom

use core::sync::atomic::Ordering;
use std::rc::Rc;

use loom::sync::Notify;
use loom::sync::atomic::AtomicU32;
use toyos::transport::Word;
use toyos_link::ends::{Driver, Stack};
use toyos_link::layout::WORDS;

/// A loom atomic as a region word: the trait is the SDK's and the type is
/// loom's, so the two meet through a wrapper.
struct Shared(AtomicU32);

impl Word for Shared {
    fn load(&self, order: Ordering) -> u32 {
        self.0.load(order)
    }
    fn store(&self, value: u32, order: Ordering) {
        self.0.store(value, order)
    }
    fn swap(&self, value: u32, order: Ordering) -> u32 {
        self.0.swap(value, order)
    }
    fn compare_exchange(&self, current: u32, new: u32, success: Ordering, failure: Ordering) -> Result<u32, u32> {
        self.0.compare_exchange(current, new, success, failure)
    }
}

/// The region's page and the stack's doorbell. Held in an `Rc`, as
/// `toyos`'s `transport_loom` holds its streams: loom's `Arc` panics again in
/// its drop when a deadlock unwinds the model, which aborts the run before it
/// says which test failed
/// (`issues/loom-arc-panics-in-its-drop-while-a-failed-model-unwinds.md`). The
/// page is built on the heap: its words on the model's 32 KiB coroutine stack
/// leave too little of it to unwind.
struct Link {
    page: Box<[Shared; WORDS]>,
    bell: Notify,
}

fn link() -> Rc<Link> {
    let words: Box<[Shared]> = (0..WORDS).map(|_| Shared(AtomicU32::new(0))).collect();
    let page = words.try_into().ok().expect("WORDS words");
    Rc::new(Link { page, bell: Notify::new() })
}

/// The stack fills the transmit ring and sleeps on its doorbell until it has
/// room; the driver takes one frame and rings only if the stack wanted it.
#[test]
fn a_full_ring_is_never_left_asleep_on_room_it_has() {
    loom::model(|| {
        let link = link();
        let mut driver = Driver::new(&link.page);
        let mut stack = Stack::new(&link.page);
        while stack.room(&link.page).unwrap() > 0 {
            stack.send(&link.page, 60).unwrap();
        }
        stack.publish(&link.page);
        let consumer = {
            let link = Rc::clone(&link);
            loom::thread::spawn(move || {
                assert!(driver.to_send(&link.page).unwrap().is_some(), "a frame published");
                if driver.release(&link.page) {
                    link.bell.notify();
                }
            })
        };
        while stack.want_room(&link.page).unwrap() == 0 {
            link.bell.wait();
        }
        consumer.join().unwrap();
    });
}
