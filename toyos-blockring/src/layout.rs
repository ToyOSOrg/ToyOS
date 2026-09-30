//! Where everything is on a session's region, in 32-bit words from its start.
//!
//! The four ring indices sit on cache lines of their own, so the client's
//! stores to its two and the server's to its two never share a line.

use toyos_transport::{Consumer, Geometry, Place, Producer, Word};

/// A session's whole region: the one size shared memory comes in.
pub const SESSION_BYTES: usize = Geometry::BYTES as usize;

/// The unit every request is in, and the unit the arena is cut into.
pub const BLOCK_BYTES: usize = 4096;

/// The arena: whole blocks after the rings' page, which a request names by
/// run.
pub const ARENA: Geometry = match Geometry::new(BLOCK_BYTES as u32) {
    Some(arena) => arena,
    None => panic!("a block is no longer than the arena"),
};

/// How many requests, and so how many completions, one session has in flight.
pub const DEPTH: u32 = 64;

/// The most blocks one request moves. A driver whose device takes less in one
/// command splits it; one that takes more is still asked for no more than this.
pub const MAX_REQUEST_BLOCKS: u32 = 32;

/// Index words. The server writes [`SQ_HEAD`] and [`CQ_TAIL`], the client the
/// other two.
pub const SQ_HEAD: usize = 0;
pub const SQ_TAIL: usize = 16;
pub const CQ_HEAD: usize = 32;
pub const CQ_TAIL: usize = 48;

/// Words per request entry, and where the request ring starts.
pub const SQE_WORDS: usize = 8;
pub const SQ_BASE: usize = 64;

/// Words per completion entry, and where the completion ring starts.
pub const CQE_WORDS: usize = 4;
pub const CQ_BASE: usize = SQ_BASE + DEPTH as usize * SQE_WORDS;

/// Every word the rings use; the page they are on is the first block.
pub const RING_WORDS: usize = CQ_BASE + DEPTH as usize * CQE_WORDS;

/// The request ring and the completion ring.
pub const REQUESTS: Place<SQE_WORDS, DEPTH, RING_WORDS> = Place::new::<SQ_HEAD, SQ_TAIL, SQ_BASE>();
pub const COMPLETIONS: Place<CQE_WORDS, DEPTH, RING_WORDS> = Place::new::<CQ_HEAD, CQ_TAIL, CQ_BASE>();

/// A client's two ends: requests out, completions in.
pub type ClientRings = (Producer<SQE_WORDS, DEPTH, RING_WORDS>, Consumer<CQE_WORDS, DEPTH, RING_WORDS>);

/// A server's two ends: requests in, completions out.
pub type ServerRings = (Consumer<SQE_WORDS, DEPTH, RING_WORDS>, Producer<CQE_WORDS, DEPTH, RING_WORDS>);

/// The client's ends of a session page, every word it owns set to 0. Done
/// before the page is sent to a server, and again before it is sent to the
/// next one.
pub fn client<W: Word>(page: &[W; RING_WORDS]) -> ClientRings {
    (Producer::new(page, REQUESTS), Consumer::new(page, COMPLETIONS))
}

/// The server's ends of a session page it was sent, every word it owns set to
/// 0. Whatever the client left in its own is bounded when first looked at.
///
/// Made before the open is answered: a client that reconnects sends the page
/// its last server wrote, and reads these two words the moment it hears.
pub fn server<W: Word>(page: &[W; RING_WORDS]) -> ServerRings {
    (Consumer::new(page, REQUESTS), Producer::new(page, COMPLETIONS))
}

const _: () = assert!(RING_WORDS * 4 <= Geometry::HEADER_BYTES as usize);
const _: () = assert!(MAX_REQUEST_BLOCKS <= ARENA.slots());
