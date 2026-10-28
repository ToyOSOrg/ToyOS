//! Where everything is on a link's region, in 32-bit words from its start.
//!
//! Each cache line of the header is written by one side: the stack's cursors
//! and requests, and the driver's cursors, state and answers, never share one.

use toyos::transport::{Geometry, Place, Run};

/// A link's whole region: `toyos::transport`'s region length.
pub const REGION_BYTES: usize = Geometry::BYTES as usize;

/// One frame's slot in the arena, and so the longest frame either way.
pub const SLOT_BYTES: usize = 2048;

/// The arena, cut into frame slots after the header page.
pub const ARENA: Geometry = match Geometry::new(SLOT_BYTES as u32) {
    Some(arena) => arena,
    None => panic!("a slot is no longer than the arena"),
};

/// Frames each ring holds. Received frame `n` is in receive slot
/// `n % RX_DEPTH`, and sent frame `n` in transmit slot `n % TX_DEPTH`: a slot
/// is its producer's again exactly when the consumer releases its entry.
pub const RX_DEPTH: u32 = 256;
pub const TX_DEPTH: u32 = 256;

/// The stack's line: the receive ring's head and the transmit ring's tail.
pub const RX_HEAD: usize = 0;
pub const TX_TAIL: usize = 16;
/// The driver's line: the receive ring's tail and the transmit ring's head.
pub const RX_TAIL: usize = 32;
pub const TX_HEAD: usize = 48;

/// The driver's state: its link ([`crate::state::Link`]), the receive ring's
/// want, which the driver sets when the ring is full and the stack takes when
/// it releases, and the counters it answered.
pub const LINK: usize = 64;
pub const RX_WANT: usize = 65;
pub const COUNTERS_ANSWERED: usize = 66;
pub const COUNTERS_PRESENT: usize = 67;

/// The stack's requests: the transmit ring's want, which the stack sets when
/// the ring is full and the driver takes when it releases; the multicast
/// groups it joined; and the counters it asked for.
pub const TX_WANT: usize = 80;
pub const GROUPS_JOINED: usize = 81;
pub const COUNTERS_ASKED: usize = 82;

/// The groups the stack joined, two words each, in the order it joined them.
pub const GROUPS: usize = 96;
pub const MAX_GROUPS: usize = 8;

/// The driver's counters, two words each, low word first
/// ([`crate::state::Counter`]).
pub const COUNTERS: usize = 128;

/// The rings' entries: one word each, the frame's length.
pub const RX_ENTRIES: usize = 256;
pub const TX_ENTRIES: usize = RX_ENTRIES + RX_DEPTH as usize;

/// Every word the header uses.
pub const WORDS: usize = TX_ENTRIES + TX_DEPTH as usize;

pub const RX: Place<1, RX_DEPTH, WORDS> = Place::new::<RX_HEAD, RX_TAIL, RX_ENTRIES>();
pub const TX: Place<1, TX_DEPTH, WORDS> = Place::new::<TX_HEAD, TX_TAIL, TX_ENTRIES>();

/// The slot of received frame `n`.
pub fn rx_slot(n: u32) -> Run {
    slot(n % RX_DEPTH)
}

/// The slot of sent frame `n`, after every receive slot.
pub fn tx_slot(n: u32) -> Run {
    slot(RX_DEPTH + n % TX_DEPTH)
}

fn slot(index: u32) -> Run {
    match ARENA.run(index, 1) {
        Some(run) => run,
        None => unreachable!("both rings' slots fit the arena"),
    }
}

const _: () = {
    assert!(WORDS * 4 <= Geometry::HEADER_BYTES as usize);
    assert!(GROUPS + 2 * MAX_GROUPS <= COUNTERS);
    assert!(COUNTERS + 2 * crate::state::Counter::ALL.len() <= RX_ENTRIES);
    assert!(RX_DEPTH + TX_DEPTH <= ARENA.slots());
};

#[cfg(test)]
mod tests {
    use super::*;

    /// No two slots of either ring overlap, and every one is inside the region.
    #[test]
    fn every_slot_is_its_own() {
        let mut spans: Vec<_> = (0..RX_DEPTH).map(rx_slot).chain((0..TX_DEPTH).map(tx_slot)).map(|r| r.span()).collect();
        spans.sort_by_key(|s| s.offset);
        for pair in spans.windows(2) {
            assert!(pair[0].offset + pair[0].len <= pair[1].offset);
        }
        let last = spans.last().unwrap();
        assert!(last.offset + last.len <= REGION_BYTES);
        assert_eq!(rx_slot(RX_DEPTH), rx_slot(0), "frame n's slot repeats a ring later");
        assert_eq!(tx_slot(TX_DEPTH + 3), tx_slot(3));
    }
}
