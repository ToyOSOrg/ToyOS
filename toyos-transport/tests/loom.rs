//! The transport's two ends on two CPUs, under loom: what no host test that
//! runs both ends on one thread can reach.
//!
//! - **Publication**: a consumer that sees a tail sees every word of the
//!   entries below it. `publish-relaxed` takes the edge away.
//! - **No lost wake**: a consumer that says it sleeps and a producer that
//!   publishes cannot both miss the other; the futex is a load of the word it
//!   waits on, and a lost wake is a consumer that slept over a publish that
//!   answered [`Wake::Busy`]. `no-wake-fence` takes the producer's fence away
//!   and `no-sleep-fence` the consumer's.
//! - **A hostile peer** leaves the honest end with entries, nothing, or a
//!   named violation, and never more entries than the ring holds. `no-clamp`
//!   believes the peer.
//!
//!   cargo test -p toyos-transport --features <control> --test loom

use core::sync::atomic::Ordering;

use loom::sync::atomic::{fence, AtomicU32};
use loom::sync::Arc;
use toyos_transport::{Consumer, Cursors, Place, Producer, Untrusted, Violation, Wake, Word};

/// A loom atomic as a region word: the trait is this crate's and the type is
/// loom's, so the two meet through a wrapper.
struct Shared(AtomicU32);

impl Word for Shared {
    fn load(&self, order: Ordering) -> u32 {
        self.0.load(order)
    }
    fn store(&self, value: u32, order: Ordering) {
        self.0.store(value, order)
    }
    fn fence() {
        fence(Ordering::SeqCst)
    }
}

const E: usize = 2;
const D: u32 = 2;
const PLACE: Place = Place { cursors: Cursors { head: 0, tail: 1, sleep: 2 }, entries: 3 };
const WORDS: usize = 3 + E * D as usize;

fn page() -> Arc<Vec<Shared>> {
    Arc::new((0..WORDS).map(|_| Shared(AtomicU32::new(0))).collect())
}

fn ends(page: &[Shared]) -> (Producer<E, D>, Consumer<E, D>) {
    (Producer::new(page, PLACE).unwrap(), Consumer::new(page, PLACE).unwrap())
}

fn entry(n: u32) -> [u32; E] {
    [100 + n, 7 * n + 3]
}

fn plain(words: [Untrusted<u32>; E]) -> [u32; E] {
    words.map(|w| w.at_most(u32::MAX.into()).unwrap() as u32)
}

/// Two entries published one at a time, read by the other end as they
/// arrive: each is whole, in order, and exactly what was written.
#[test]
fn a_published_entry_is_read_whole() {
    loom::model(|| {
        let page = page();
        let (mut tx, mut rx) = ends(&page);
        let consumer_page = Arc::clone(&page);
        let consumer = loom::thread::spawn(move || {
            let mut read = Vec::new();
            while read.len() < 2 {
                match rx.pop(&consumer_page).expect("the producer keeps the protocol") {
                    Some(words) => read.push(plain(words)),
                    None => loom::thread::yield_now(),
                }
            }
            rx.release(&consumer_page).unwrap();
            read
        });
        for n in 0..2 {
            assert!(tx.push(&page, entry(n)).unwrap());
            let _ = tx.publish(&page).unwrap();
        }
        let read = consumer.join().expect("the consumer thread");
        assert_eq!(read, [entry(0), entry(1)], "an entry was read before its words");
    });
}

/// A consumer that finds nothing sleeps as a futex does — only while the tail
/// still holds what it saw — and is woken only when a publish answers
/// [`Wake::Peer`]. Whatever the schedule, a consumer that slept is woken.
#[test]
fn a_publish_and_a_sleep_cannot_both_miss() {
    loom::model(|| {
        let page = page();
        let (mut tx, mut rx) = ends(&page);
        let consumer_page = Arc::clone(&page);
        let consumer = loom::thread::spawn(move || {
            if rx.pop(&consumer_page).unwrap().is_some() {
                return false;
            }
            rx.before_sleep(&consumer_page)
                .unwrap()
                .is_some_and(|asleep| consumer_page[asleep.word].load(Ordering::Relaxed) == asleep.value)
        });
        assert!(tx.push(&page, entry(1)).unwrap());
        let wake = tx.publish(&page).unwrap();
        let slept = consumer.join().expect("the consumer thread");
        assert!(
            !slept || wake == Some(Wake::Peer),
            "the consumer slept over a published entry and the publish answered {wake:?}"
        );
    });
}

/// A producer that stores a tail past the ring, one behind what was released,
/// and garbage into the entries and into the consumer's own head and `sleep`:
/// every pop is an entry, nothing, or [`Violation::TailPastDepth`], and no
/// more than the ring's depth of entries is taken without a release.
#[test]
fn a_hostile_producer_yields_entries_or_a_violation() {
    loom::model(|| {
        let page = page();
        let (_, mut rx) = ends(&page);
        let hostile_page = Arc::clone(&page);
        let hostile = loom::thread::spawn(move || {
            for (at, value) in [(1, D + 1), (3, 9), (0, 5), (1, u32::MAX), (2, 7)] {
                hostile_page[at].store(value, Ordering::Release);
            }
        });
        let mut taken = 0;
        for _ in 0..D + 2 {
            match rx.pop(&page) {
                Ok(Some(_)) => taken += 1,
                Ok(None) => {}
                Err(violation) => {
                    assert_eq!(violation, Violation::TailPastDepth);
                    break;
                }
            }
        }
        hostile.join().unwrap();
        assert!(taken <= D, "took {taken} entries from a ring of {D} without releasing one");
    });
}

/// A consumer that stores a head past what was published, one wrapped far
/// behind, and garbage into the producer's own tail and the entries: every
/// push is room, a full ring, or [`Violation::HeadPastTail`], and no more
/// than the ring's depth is ever pushed ahead of a head that was never moved.
#[test]
fn a_hostile_consumer_yields_room_or_a_violation() {
    loom::model(|| {
        let page = page();
        let (mut tx, _) = ends(&page);
        let hostile_page = Arc::clone(&page);
        let hostile = loom::thread::spawn(move || {
            for (at, value) in [(0, 5), (1, 9), (0, u32::MAX), (2, 1), (4, 6)] {
                hostile_page[at].store(value, Ordering::Release);
            }
        });
        let mut pushed = 0;
        for n in 0..D + 2 {
            match tx.push(&page, entry(n)) {
                Ok(true) => pushed += 1,
                Ok(false) => {}
                Err(violation) => {
                    assert_eq!(violation, Violation::HeadPastTail);
                    break;
                }
            }
            let _ = tx.publish(&page).unwrap();
        }
        hostile.join().unwrap();
        assert!(pushed <= D, "pushed {pushed} into a ring of {D} nobody released");
    });
}
