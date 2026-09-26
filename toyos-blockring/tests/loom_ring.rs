//! The rings' publication edge under loom: a consumer that sees a tail sees
//! every word of the entries below it.
//!
//! The client and the server are two processes on two CPUs over one shared
//! page, so this is the one property of the rings no host test that runs both
//! ends on one thread can reach. `mutate-ring-publish-relaxed` takes the edge
//! away and this must red:
//!
//!   cargo test -p toyos-blockring --features mutate-ring-publish-relaxed --test loom_ring

use core::sync::atomic::Ordering;

use loom::sync::atomic::AtomicU32;
use loom::sync::Arc;
use toyos_blockring::entry::{Op, Request};
use toyos_blockring::layout::RING_WORDS;
use toyos_blockring::ring::{self, Word};

/// A loom atomic as a page word: the trait is this crate's and the type is
/// loom's, so the two meet through a wrapper.
struct Shared(AtomicU32);

impl Word for Shared {
    fn load(&self, order: Ordering) -> u32 {
        self.0.load(order)
    }
    fn store(&self, value: u32, order: Ordering) {
        self.0.store(value, order)
    }
}

fn page() -> Arc<Vec<Shared>> {
    Arc::new((0..RING_WORDS).map(|_| Shared(AtomicU32::new(0))).collect())
}

const PARTITION: u64 = 1 << 20;

fn request(n: u32) -> Request {
    Request { op: Op::Write, tag: 100 + n, lba: 7 + u64::from(n), blocks: 1 + n, arena: 3 * n }
}

/// Two requests published one at a time, read by the other end as they
/// arrive: each is whole, in order, and exactly what was written.
#[test]
fn a_published_request_is_read_whole() {
    loom::model(|| {
        let page = page();
        let server_page = Arc::clone(&page);
        let server = loom::thread::spawn(move || {
            let (mut requests, _) = ring::server(&server_page);
            let mut read = Vec::new();
            while read.len() < 2 {
                match requests.pop().expect("the client keeps the protocol") {
                    Some(words) => read.push(Request::decode(words, PARTITION)),
                    None => loom::thread::yield_now(),
                }
            }
            requests.release();
            read
        });
        let (mut requests, _) = ring::client(&page);
        for n in 0..2 {
            requests.push(request(n).encode());
            requests.publish();
        }
        let read = server.join().expect("the server thread");
        assert_eq!(read, [Ok(request(0)), Ok(request(1))], "a request was read before its words");
    });
}
