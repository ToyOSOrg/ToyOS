//! The input context of the Configure Endpoint that creates a device's
//! endpoints, and of the one that re-creates a mass-storage device's bulk
//! pair.
//!
//! Every dword the command reads is decided here and handed to the driver as a
//! list it writes without looking inside, so there is no flag the driver can
//! leave out: what reaches the controller is what these tests read.
//!
//! **One description for the bind and the recovery.** A recovery that described
//! the endpoints differently from the bind — another burst, another packet
//! size — would be a second, disagreeing device on the same slot. The two
//! differ in the control context's Drop flags and in the slot context, and
//! nowhere else.

use alloc::vec::Vec;

/// One dword of an input context: `context` 0 is the Input Control Context, 1
/// the Slot Context, `dci + 1` an endpoint's (xHCI 1.2 §6.2.5).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Word {
    pub context: usize,
    pub dword: usize,
    pub value: u32,
}

/// EP Type (Table 6-9).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    BulkOut = 2,
    InterruptOut = 3,
    BulkIn = 6,
    InterruptIn = 7,
}

/// One endpoint as its descriptor gave it, and the ring it starts on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Endpoint {
    pub dci: u8,
    pub kind: Kind,
    pub max_packet: u16,
    pub max_burst: u8,
    /// The context's Interval exponent ([`interval`]); 0 for bulk.
    pub interval: u8,
    pub dequeue: u64,
}

/// Whether the command creates the endpoints or re-creates them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Configure {
    /// The bind. The slot context names the speed the port trained at and the
    /// root-hub port, numbered from 1.
    First { speed: u8, port: u8 },
    /// A recovery: each endpoint's Drop flag is set beside its Add flag, the
    /// form that zeroes an endpoint's data toggle or sequence number (§4.6.8's
    /// note). The slot context is what §6.2.2.2 makes valid for the command —
    /// Context Entries, and the Hub field, which for a function is 0 and takes
    /// Number of Ports with it — with USB Device Address and Slot State
    /// initialised to 0 as Table 6-7 requires of an input.
    Again,
}

/// CErr: three tries before a USB Transaction Error halts the endpoint.
const CERR: u32 = 3;

/// The Interval exponent an interrupt endpoint's context carries for its
/// descriptor's `bInterval` at port speed `speed` (§6.2.3.6): frames at full
/// and low speed, read as 2^Interval × 125 µs rounded down and held to 3..=10;
/// an exponent plus one at high speed and above, held to 0..=15.
pub fn interval(speed: u8, b_interval: u8) -> u8 {
    match speed {
        1 | 2 => {
            let microframes = u32::from(b_interval.max(1)) * 8;
            (31 - microframes.leading_zeros()).clamp(3, 10) as u8
        }
        _ => b_interval.clamp(1, 16) - 1,
    }
}

/// Every dword of the input context, in the order it is written.
pub fn endpoints(eps: &[Endpoint], configure: Configure) -> Vec<Word> {
    let flags = eps.iter().fold(0u32, |all, ep| all | 1 << ep.dci);
    let entries = u32::from(eps.iter().map(|ep| ep.dci).max().unwrap_or(1)) << 27;
    let (drop, slot) = match configure {
        Configure::First { speed, port } => (0, [(u32::from(speed) << 20) | entries, u32::from(port) << 16, 0, 0]),
        Configure::Again => (flags, [entries, 0, 0, 0]),
    };
    let word = |context, dword, value| Word { context, dword, value };
    // D0 and D1 stay clear, and A0 names the slot context every Configure
    // Endpoint evaluates (§6.2.5.1).
    let mut words = alloc::vec![
        word(0, 0, drop),
        word(0, 1, 1 | flags),
        word(1, 0, slot[0]),
        word(1, 1, slot[1]),
        word(1, 2, slot[2]),
        word(1, 3, slot[3]),
    ];
    for ep in eps {
        let context = usize::from(ep.dci) + 1;
        let payload = u32::from(ep.max_packet) * (u32::from(ep.max_burst) + 1);
        let periodic = matches!(ep.kind, Kind::InterruptIn | Kind::InterruptOut);
        words.extend([
            word(context, 0, u32::from(ep.interval) << 16),
            word(
                context,
                1,
                (CERR << 1) | ((ep.kind as u32) << 3) | (u32::from(ep.max_burst) << 8) | (u32::from(ep.max_packet) << 16),
            ),
            word(context, 2, ep.dequeue as u32),
            word(context, 3, (ep.dequeue >> 32) as u32),
            // Average TRB Length is advisory: the packet size stands in for
            // it. A periodic endpoint's Max ESIT Payload is what one service
            // interval moves (§6.2.3.8).
            word(context, 4, u32::from(ep.max_packet) | if periodic { payload << 16 } else { 0 }),
        ]);
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    const IN: Endpoint =
        Endpoint { dci: 3, kind: Kind::BulkIn, max_packet: 512, max_burst: 0, interval: 0, dequeue: 0x1_2345_6001 };
    const OUT: Endpoint =
        Endpoint { dci: 4, kind: Kind::BulkOut, max_packet: 1024, max_burst: 15, interval: 0, dequeue: 0xABCD_E001 };
    const FIRST: Configure = Configure::First { speed: 4, port: 7 };

    fn bulk_pair(in_ep: Endpoint, out_ep: Endpoint, configure: Configure) -> Vec<Word> {
        endpoints(&[out_ep, in_ep], configure)
    }

    fn read(words: &[Word], context: usize, dword: usize) -> u32 {
        let mut found = words.iter().filter(|w| w.context == context && w.dword == dword);
        let word = found.next().unwrap_or_else(|| panic!("context {context} dword {dword} is never written"));
        assert!(found.next().is_none(), "context {context} dword {dword} is written twice");
        word.value
    }

    /// §4.6.8's note: the toggle or sequence number is zeroed by a Configure
    /// Endpoint with the endpoint's Drop *and* Add flags set. Add alone is
    /// another command.
    #[test]
    fn a_recovery_drops_and_adds_both_endpoints() {
        let words = bulk_pair(IN, OUT, Configure::Again);
        assert_eq!(read(&words, 0, 0), (1 << 3) | (1 << 4), "Drop Context flags");
        assert_eq!(read(&words, 0, 1), 1 | (1 << 3) | (1 << 4), "Add Context flags");
    }

    /// §6.2.5.1: D0 and D1 are reserved, and nothing but the pair is dropped.
    #[test]
    fn nothing_but_the_pair_is_ever_dropped() {
        for configure in [FIRST, Configure::Again] {
            let drop = read(&bulk_pair(IN, OUT, configure), 0, 0);
            assert_eq!(drop & 0b11, 0, "{configure:?} sets D0 or D1");
            assert_eq!(drop & !((1 << 3) | (1 << 4)), 0, "{configure:?} drops a stranger");
        }
    }

    /// An endpoint that does not exist yet has nothing to drop.
    #[test]
    fn a_bind_adds_and_drops_nothing() {
        let words = bulk_pair(IN, OUT, FIRST);
        assert_eq!(read(&words, 0, 0), 0);
        assert_eq!(read(&words, 0, 1), 1 | (1 << 3) | (1 << 4));
    }

    /// §6.2.2.2 and Table 6-7, field by field: Context Entries is the last
    /// endpoint's index; Hub, Number of Ports, USB Device Address and Slot
    /// State are 0; and nothing else is set.
    #[test]
    fn a_recoverys_slot_context_is_context_entries_and_zeroes() {
        let words = bulk_pair(IN, OUT, Configure::Again);
        let dword0 = read(&words, 1, 0);
        assert_eq!(dword0 >> 27, 4, "Context Entries");
        assert_eq!(dword0 & (1 << 26), 0, "Hub");
        assert_eq!(dword0 & ((1 << 27) - 1), 0, "every field of dword 0 below Context Entries");
        assert_eq!(read(&words, 1, 1) >> 24, 0, "Number of Ports");
        assert_eq!(read(&words, 1, 1), 0);
        assert_eq!(read(&words, 1, 2), 0);
        let dword3 = read(&words, 1, 3);
        assert_eq!(dword3 & 0xFF, 0, "USB Device Address");
        assert_eq!(dword3 >> 27, 0, "Slot State");
        assert_eq!(dword3, 0);
    }

    /// Context Entries follows the higher of the two indexes, whichever pipe
    /// holds it.
    #[test]
    fn context_entries_is_the_higher_index_of_the_pair() {
        let swapped = (Endpoint { dci: 9, ..IN }, Endpoint { dci: 2, ..OUT });
        for configure in [FIRST, Configure::Again] {
            assert_eq!(read(&bulk_pair(swapped.0, swapped.1, configure), 1, 0) >> 27, 9);
        }
    }

    #[test]
    fn a_binds_slot_context_names_the_speed_and_the_port() {
        let words = bulk_pair(IN, OUT, FIRST);
        assert_eq!(read(&words, 1, 0), (4 << 20) | (4 << 27));
        assert_eq!(read(&words, 1, 1), 7 << 16);
        assert_eq!(read(&words, 1, 2), 0);
        assert_eq!(read(&words, 1, 3), 0);
    }

    /// The pair is described to the controller identically both times.
    #[test]
    fn the_recovery_describes_the_endpoints_exactly_as_the_bind_did() {
        let (first, again) = (bulk_pair(IN, OUT, FIRST), bulk_pair(IN, OUT, Configure::Again));
        for context in [IN.dci as usize + 1, OUT.dci as usize + 1] {
            for dword in 0..5 {
                assert_eq!(read(&first, context, dword), read(&again, context, dword));
            }
        }
    }

    /// Table 6-9, against numbers worked by hand: CErr 3 in bits 2:1, EP Type
    /// in 5:3, Max Burst Size in 15:8, Max Packet Size in 31:16.
    #[test]
    fn an_endpoint_context_carries_the_descriptors_own_numbers() {
        let words = bulk_pair(IN, OUT, Configure::Again);
        assert_eq!(read(&words, 4, 1), 0x0200_0036, "Bulk In, 512 B, burst 0");
        assert_eq!(read(&words, 5, 1), 0x0400_0F16, "Bulk Out, 1024 B, burst 15");
        assert_eq!(read(&words, 4, 2), 0x2345_6001);
        assert_eq!(read(&words, 4, 3), 0x1);
        assert_eq!(read(&words, 5, 2), 0xABCD_E001);
        assert_eq!(read(&words, 5, 3), 0);
        assert_eq!(read(&words, 4, 4), 512);
        assert_eq!(read(&words, 5, 4), 1024);
    }

    /// §6.2.3.6, against numbers worked by hand: a full-speed keyboard of
    /// bInterval 10 ms is serviced every 2^6 × 125 µs = 8 ms, never slower
    /// than asked; a high-speed one of bInterval 4 every 2^3 microframes.
    #[test]
    fn an_interrupt_endpoints_interval_is_the_specifications_exponent() {
        assert_eq!(interval(1, 10), 6);
        assert_eq!(interval(2, 1), 3);
        assert_eq!(interval(1, 255), 10);
        assert_eq!(interval(3, 4), 3);
        assert_eq!(interval(4, 1), 0);
        assert_eq!(interval(3, 0), 0);
        let kb = Endpoint { dci: 3, kind: Kind::InterruptIn, max_packet: 8, max_burst: 0, interval: 6, dequeue: 0x1000 };
        let words = endpoints(&[kb], FIRST);
        assert_eq!(read(&words, 4, 0), 6 << 16);
        assert_eq!(read(&words, 4, 1), (8 << 16) | (7 << 3) | (3 << 1));
        assert_eq!(read(&words, 4, 4), (8 << 16) | 8);
    }
}
