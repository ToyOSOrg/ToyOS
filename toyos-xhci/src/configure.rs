//! The input context of the Configure Endpoint that creates a mass-storage
//! device's bulk pair, and of the one that re-creates it.
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

/// One dword of an input context: `context` 0 is the Input Control Context, 1
/// the Slot Context, `dci + 1` an endpoint's (xHCI 1.2 §6.2.5).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Word {
    pub context: usize,
    pub dword: usize,
    pub value: u32,
}

/// One bulk endpoint as its descriptor gave it, and the ring it starts on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BulkEndpoint {
    pub dci: u8,
    pub max_packet: u16,
    pub max_burst: u8,
    pub dequeue: u64,
}

/// Whether the command creates the pair or re-creates it.
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

/// How many dwords [`bulk_pair`] decides: two of the control context, four of
/// the slot context, five of each endpoint context.
pub const WORDS: usize = 16;

/// EP Type (Table 6-9): 2 is Bulk Out, 6 is Bulk In.
const BULK_OUT: u32 = 2;
const BULK_IN: u32 = 6;
/// CErr: three tries before a USB Transaction Error halts the endpoint.
const CERR: u32 = 3;

/// Every dword of the input context, in the order it is written.
pub fn bulk_pair(in_ep: BulkEndpoint, out_ep: BulkEndpoint, configure: Configure) -> [Word; WORDS] {
    let endpoints = (1u32 << in_ep.dci) | (1u32 << out_ep.dci);
    let entries = (in_ep.dci.max(out_ep.dci) as u32) << 27;
    let (drop, slot) = match configure {
        Configure::First { speed, port } => {
            (0, [((speed as u32) << 20) | entries, (port as u32) << 16, 0, 0])
        }
        Configure::Again => (endpoints, [entries, 0, 0, 0]),
    };
    let word = |context, dword, value| Word { context, dword, value };
    let endpoint = |ep: BulkEndpoint, ep_type: u32| {
        let context = ep.dci as usize + 1;
        [
            word(context, 0, 0),
            word(
                context,
                1,
                (CERR << 1) | (ep_type << 3) | ((ep.max_burst as u32) << 8) | ((ep.max_packet as u32) << 16),
            ),
            word(context, 2, ep.dequeue as u32),
            word(context, 3, (ep.dequeue >> 32) as u32),
            // Average TRB Length is advisory; the endpoint's own packet size
            // stands in for it.
            word(context, 4, ep.max_packet as u32),
        ]
    };
    let [o0, o1, o2, o3, o4] = endpoint(out_ep, BULK_OUT);
    let [i0, i1, i2, i3, i4] = endpoint(in_ep, BULK_IN);
    [
        // D0 and D1 stay clear, and A0 names the slot context every Configure
        // Endpoint evaluates (§6.2.5.1).
        word(0, 0, drop),
        word(0, 1, 1 | endpoints),
        word(1, 0, slot[0]),
        word(1, 1, slot[1]),
        word(1, 2, slot[2]),
        word(1, 3, slot[3]),
        o0, o1, o2, o3, o4, i0, i1, i2, i3, i4,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const IN: BulkEndpoint =
        BulkEndpoint { dci: 3, max_packet: 512, max_burst: 0, dequeue: 0x1_2345_6001 };
    const OUT: BulkEndpoint =
        BulkEndpoint { dci: 4, max_packet: 1024, max_burst: 15, dequeue: 0xABCD_E001 };
    const FIRST: Configure = Configure::First { speed: 4, port: 7 };

    fn read(words: &[Word; WORDS], context: usize, dword: usize) -> u32 {
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
        let swapped = (BulkEndpoint { dci: 9, ..IN }, BulkEndpoint { dci: 2, ..OUT });
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
}
