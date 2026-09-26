//! Where everything is on a session's region, in 32-bit words from its start.
//!
//! The four ring indices sit on cache lines of their own, so the client's
//! stores to its two and the server's to its two never share a line.

/// A session's whole region: the one size shared memory comes in.
pub const SESSION_BYTES: usize = 2 * 1024 * 1024;

/// The unit every request is in, and the unit the arena is cut into.
pub const BLOCK_BYTES: usize = 4096;

/// How many requests, and so how many completions, one session has in flight.
/// A power of two, so an index is its ring position masked.
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

/// Where the arena starts, in bytes: the block after the rings' page.
pub const ARENA_OFFSET: usize = BLOCK_BYTES;

/// The arena's blocks; a request's `arena` is an index below this.
pub const ARENA_BLOCKS: u32 = ((SESSION_BYTES - ARENA_OFFSET) / BLOCK_BYTES) as u32;

const _: () = assert!(DEPTH.is_power_of_two());
const _: () = assert!(RING_WORDS * 4 <= ARENA_OFFSET);
const _: () = assert!(MAX_REQUEST_BLOCKS <= ARENA_BLOCKS);

/// The byte offset of arena block `block` in the region.
pub const fn arena_byte(block: u32) -> usize {
    ARENA_OFFSET + block as usize * BLOCK_BYTES
}
