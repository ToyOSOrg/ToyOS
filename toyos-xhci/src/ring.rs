//! Where a transfer ring stands: the controller's dequeue pointer against the
//! driver's own enqueue point.
//!
//! The controller says how far it has got in the endpoint's output context
//! (xHCI 1.2 Table 6-8, dwords 2-3) as an *address* on the ring, never an index
//! into it. Turning that back into a position is arithmetic over a number the
//! driver did not write, so it is here and it refuses by name: the one reader
//! is a reset path that may not panic, and a pointer that is no position on
//! this ring has to be said rather than wrapped into a plausible count.

/// One TRB's width (xHCI 1.2 §4.11.1), which is also the alignment every
/// address on a ring has.
const TRB_BYTES: u64 = 16;

/// One transfer ring, as the numbers a position is computed from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Ring {
    /// The ring's first TRB, in the address space the controller was programmed
    /// in.
    pub base: u64,
    /// TRBs the ring holds, its link TRB included.
    pub trbs: u16,
    /// Where the driver's next enqueue goes, as an index into the ring.
    pub tail: u16,
}

/// Why a dequeue pointer says nothing about how far a controller had got.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NotOnTheRing {
    /// Before the ring's first TRB, or past its last — which is what a context
    /// the controller never wrote, or one published mid-update, reads as.
    Dequeue(u64),
    /// The driver's own next enqueue point is past the end of its ring.
    Tail(u16),
}

impl core::fmt::Display for NotOnTheRing {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Dequeue(at) => write!(f, "a dequeue pointer of {at:#x}, which is no TRB on that ring"),
            Self::Tail(tail) => write!(f, "an enqueue point of {tail}, which is past the end of that ring"),
        }
    }
}

impl Ring {
    /// TRBs the driver has queued that the controller has not reached, out of
    /// the Endpoint Context's TR Dequeue Pointer field.
    ///
    /// The field's low four bits are the Dequeue Cycle State and three reserved
    /// bits (xHCI 1.2 §6.2.3) and not address, so they are masked off before
    /// anything is decided about where it points.
    pub fn pending(self, dequeue: u64) -> Result<u16, NotOnTheRing> {
        if self.tail >= self.trbs {
            return Err(NotOnTheRing::Tail(self.tail));
        }
        let at = dequeue & !(TRB_BYTES - 1);
        let Some(off) = at.checked_sub(self.base) else {
            return Err(NotOnTheRing::Dequeue(dequeue));
        };
        let trbs = u64::from(self.trbs);
        // A base that is not itself a TRB boundary puts every position on this
        // ring between two, which is no TRB on it either.
        if !off.is_multiple_of(TRB_BYTES) || off / TRB_BYTES >= trbs {
            return Err(NotOnTheRing::Dequeue(dequeue));
        }
        let reached = off / TRB_BYTES;
        // Ring positions and not addresses: the driver's tail is ahead of the
        // controller's, and the distance between them wraps with the ring.
        Ok(((u64::from(self.tail) + trbs - reached) % trbs) as u16)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bulk ring as this tree builds one: a page of TRBs at a page-aligned
    /// address.
    fn ring(tail: u16) -> Ring {
        Ring { base: 0x1_0000, trbs: 256, tail }
    }

    /// The address of ring index `at`.
    fn trb(at: u64) -> u64 {
        0x1_0000 + at * 16
    }

    #[test]
    fn a_controller_that_has_caught_up_has_nothing_pending() {
        for at in [0, 1, 17, 200, 255] {
            assert_eq!(ring(at as u16).pending(trb(at)), Ok(0), "index {at}");
        }
    }

    /// The count is the distance from where the controller is to where the
    /// driver's next TRB goes, and it wraps: a dequeue pointer *ahead* of the
    /// tail is the ordinary state of a ring that has been round once.
    #[test]
    fn the_count_is_the_wrapped_distance_between_the_two() {
        for (tail, reached, want) in
            [(20u64, 0u64, 20u16), (0, 20, 236), (255, 254, 1), (10, 200, 66), (0, 1, 255)]
        {
            assert_eq!(
                ring(tail as u16).pending(trb(reached)),
                Ok(want),
                "tail {tail}, reached {reached}"
            );
        }
    }

    /// The Dequeue Cycle State and the three bits beside it ride in the same
    /// field; a decode that took them for address would answer about a TRB the
    /// controller is not on.
    #[test]
    fn the_low_four_bits_of_the_field_are_not_address() {
        for noise in 0..16u64 {
            assert_eq!(ring(20).pending(trb(0) | noise), Ok(20), "low bits {noise}");
        }
    }

    /// **The refusal this exists for.** A context the controller never wrote,
    /// or one read while another CPU was publishing it, hands back a word that
    /// is not on this ring at all.
    #[test]
    fn a_pointer_that_is_no_position_on_this_ring_is_refused_by_name() {
        let r = ring(20);
        for stray in [0, 0xF, 0x8000, trb(0) - 16, trb(256), trb(4096), u64::MAX] {
            assert_eq!(r.pending(stray), Err(NotOnTheRing::Dequeue(stray)), "{stray:#x}");
        }
        // The two ends, either side of the boundary: the last TRB is on the
        // ring and the one after it is not.
        assert_eq!(r.pending(trb(255)), Ok(21));
        assert_eq!(r.pending(trb(256)), Err(NotOnTheRing::Dequeue(trb(256))));
    }

    /// The driver's own half of the pair, refused for the same reason: a tail
    /// off the end of the ring makes the distance meaningless, and a modulo
    /// would hide it.
    #[test]
    fn an_enqueue_point_past_the_end_of_the_ring_is_refused() {
        for tail in [256u16, 257, u16::MAX] {
            let r = Ring { base: 0x1_0000, trbs: 256, tail };
            assert_eq!(r.pending(trb(0)), Err(NotOnTheRing::Tail(tail)));
        }
        // And a ring of no TRBs is that same refusal rather than a division by
        // zero.
        assert_eq!(
            Ring { base: 0x1_0000, trbs: 0, tail: 0 }.pending(trb(0)),
            Err(NotOnTheRing::Tail(0))
        );
    }

    /// Each refusal says which of the two it is, because the account is the
    /// only place a reader learns anything about that reset at all.
    #[test]
    fn no_two_refusals_read_the_same() {
        extern crate std;
        use std::string::ToString;
        let said = [
            NotOnTheRing::Dequeue(0x20).to_string(),
            NotOnTheRing::Tail(300).to_string(),
        ];
        for (at, one) in said.iter().enumerate() {
            assert!(!one.is_empty());
            for other in &said[at + 1..] {
                assert_ne!(one, other);
            }
        }
    }
}
