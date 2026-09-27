//! Whose fault a trap was — the one classification the crash path makes. A
//! Ring 3 fault is its process's, and the process dies; any Ring 0 fault is the
//! kernel's, whatever thread or syscall is current, and the machine halts.
//!
//! **The ring is a fact the frame carries, and this file exists so that nothing
//! guesses it from anything else**, such as a faulting address.

/// The privilege level a trap frame arrived from.
///
/// Opaque, and there is one constructor: the frame's own `cs`. A `Ring` is
/// therefore never anything but what the hardware pushed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Ring(bool);

impl Ring {
    /// The ring a code segment selector names. The RPL field is the low two
    /// bits, and this kernel runs Ring 0 and Ring 3 only.
    pub const fn of_cs(cs: u64) -> Self {
        Self(cs & 3 != 0)
    }

    /// Whether the frame was Ring 3.
    pub const fn is_user(self) -> bool {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Ring 3 code selector, and a Ring 0 one.
    const USER_CS: u64 = 0x2B;
    const KERNEL_CS: u64 = 0x08;

    #[test]
    fn a_ring_is_only_ever_what_cs_said() {
        assert!(Ring::of_cs(USER_CS).is_user());
        assert!(!Ring::of_cs(KERNEL_CS).is_user());
        // Every selector with a non-zero RPL is Ring 3 and no other, so a
        // faulting address cannot enter this answer at all.
        for rpl in 1..4u64 {
            assert!(Ring::of_cs(rpl).is_user(), "RPL {rpl} is not Ring 0");
        }
        assert!(!Ring::of_cs(0).is_user());
    }
}
