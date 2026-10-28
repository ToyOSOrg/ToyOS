//! A full ring's want on two CPUs, under loom: the producer that finds no room
//! and the consumer that releases one never both miss each other, so the
//! producer is never left asleep on room it has.
//!
//!   cargo test -p toyos-link --features mutate-want-relaxed --test ends_loom

use core::sync::atomic::Ordering;

use loom::sync::atomic::AtomicU32;
use loom::sync::{Arc, Notify};
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

/// The stack fills the transmit ring and sleeps on its doorbell until it has
/// room; the driver takes one frame and rings only if the stack wanted it.
#[test]
fn a_full_ring_is_never_left_asleep_on_room_it_has() {
    loom::model(|| {
        let page: Arc<[Shared; WORDS]> = Arc::new(core::array::from_fn(|_| Shared(AtomicU32::new(0))));
        let mut driver = Driver::new(&page);
        let mut stack = Stack::new(&page);
        while stack.room(&page).unwrap() > 0 {
            stack.send(&page, 60).unwrap();
        }
        stack.publish(&page);
        let bell = Arc::new(Notify::new());
        let consumer = {
            let (page, bell) = (Arc::clone(&page), Arc::clone(&bell));
            loom::thread::spawn(move || {
                assert!(driver.to_send(&page).unwrap().is_some(), "a frame published");
                if driver.release(&page) {
                    bell.notify();
                }
            })
        };
        while stack.want_room(&page).unwrap() == 0 {
            bell.wait();
        }
        consumer.join().unwrap();
    });
}
