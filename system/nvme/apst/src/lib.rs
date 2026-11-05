//! The Autonomous Power State Transition table for one NVMe controller, built
//! from its own power state descriptors (NVM Express Base Specification 2.1:
//! Identify Controller, Figure 312; the descriptor, Figure 313; the feature,
//! §5.1.25.1.6, Figures 392 and 393; power management, §8.1.17).
//!
//! **The controller is let into a non-operational state only where the
//! latency it states for the round trip fits [`EXIT_BOUND_US`].** A
//! transition costs the old state's exit latency plus the new state's entry
//! latency (§8.1.17), so the first I/O after an idle period waits at most the
//! sum of the target's entry and exit latencies. A state that does not report
//! both (`0h` is "not reported") has no stated bound and is never a target.
//!
//! **Each state goes to the deepest admitted state of the first [`TIERS`]
//! tier that holds one deeper than itself**, by round-trip latency, reporting
//! no higher idle power, after that tier's idle time: a state within 15 ms
//! after 100 ms idle, one within 100 ms after 2 s. These are Linux's tiers,
//! so a drive idles as it does under Linux. A drive's table is taken as it
//! states it; there is no table of drives.

#![forbid(unsafe_code)]

/// Identify Controller's bytes this reads: NPSS at 263, APSTA at 265, and
/// the 32 power state descriptors from 2048, 32 bytes each (Figure 312).
pub const NPSS: usize = 263;
pub const APSTA: usize = 265;
pub const DESCRIPTORS: usize = 2048;
pub const DESCRIPTOR_BYTES: usize = 32;
pub const STATES: usize = 32;

/// The most the first I/O after idle may wait for the drive's power state
/// round trip, in microseconds: under a tenth of a second, below what a
/// person notices as a pause.
pub const EXIT_BOUND_US: u64 = 100_000;

/// The tiers, shallowest first: the most round-trip latency a target may
/// have, in microseconds, and the idle time before the transition, in
/// milliseconds. The last tier's latency is [`EXIT_BOUND_US`].
pub const TIERS: [(u64, u64); 2] = [(15_000, 100), (EXIT_BOUND_US, 2_000)];

/// One power state as its descriptor states it (Figure 313).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PowerState {
    /// Non-Operational State: no I/O is processed in it.
    pub non_operational: bool,
    /// Entry and exit latency, µs; `0` is not reported.
    pub entry_us: u32,
    pub exit_us: u32,
    /// Idle power in units of 0.1 mW, `None` where not reported.
    pub idle_power: Option<u64>,
    /// Maximum power in units of 0.1 mW, `None` where not reported.
    pub max_power: Option<u64>,
}

impl PowerState {
    /// The descriptor's 32 bytes.
    pub fn parse(d: &[u8; DESCRIPTOR_BYTES]) -> Self {
        let u16_at = |at: usize| u16::from_le_bytes([d[at], d[at + 1]]);
        let u32_at = |at: usize| u32::from_le_bytes([d[at], d[at + 1], d[at + 2], d[at + 3]]);
        let flags = d[3];
        // MP, bits 15:00, scaled by MXPS (bit 24): 0.01 W or 0.0001 W.
        let mp = u64::from(u16_at(0));
        let max_power = (mp != 0).then(|| if flags & 1 != 0 { mp } else { mp * 100 });
        // IDLP, bits 143:128, scaled by IPS (bits 151:150).
        let idlp = u64::from(u16_at(16));
        let idle_power = match (idlp, d[18] >> 6) {
            (0, _) => None,
            (p, 0b01) => Some(p),
            (p, 0b10) => Some(p * 100),
            _ => None,
        };
        Self { non_operational: flags & 0b10 != 0, entry_us: u32_at(4), exit_us: u32_at(8), idle_power, max_power }
    }

    /// The round trip into and out of this state, µs, where both are stated.
    pub fn round_trip_us(&self) -> Option<u64> {
        (self.entry_us != 0 && self.exit_us != 0).then(|| u64::from(self.entry_us) + u64::from(self.exit_us))
    }

    /// Whether this state may be a transition's target at all.
    fn admitted(&self) -> bool {
        self.non_operational && self.round_trip_us().is_some_and(|t| t <= EXIT_BOUND_US)
    }
}

/// The controller's states from its Identify Controller data, and whether it
/// supports APST; `None` for data too short to hold them.
pub fn states(identify: &[u8]) -> Option<(bool, Vec<PowerState>)> {
    if identify.len() < DESCRIPTORS + STATES * DESCRIPTOR_BYTES {
        return None;
    }
    let count = usize::from(identify[NPSS]) + 1;
    let states = identify[DESCRIPTORS..DESCRIPTORS + count * DESCRIPTOR_BYTES]
        .as_chunks::<DESCRIPTOR_BYTES>()
        .0
        .iter()
        .map(PowerState::parse)
        .collect();
    Some((identify[APSTA] & 1 != 0, states))
}

/// One entry of the table: the state to move to and the idle time before
/// it, or `None` for no transition out of this state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transition {
    pub to: u8,
    pub after_ms: u64,
}

/// Each state's transition, in state order.
pub fn plan(states: &[PowerState]) -> [Option<Transition>; STATES] {
    let mut out = [None; STATES];
    for (n, from) in states.iter().enumerate() {
        let floor = if from.non_operational { from.round_trip_us() } else { None };
        let deeper: Vec<(usize, u64)> = states
            .iter()
            .enumerate()
            .filter(|&(t, to)| t != n && to.admitted())
            .filter_map(|(t, to)| Some((t, to, to.round_trip_us()?)))
            .filter(|&(_, _, trip)| floor.is_none_or(|f| trip > f))
            .filter(|(_, to, _)| match (to.idle_power, from.idle_power) {
                (Some(to), Some(from)) => to <= from,
                _ => true,
            })
            .map(|(t, _, trip)| (t, trip))
            .collect();
        out[n] = TIERS.iter().find_map(|&(within_us, after_ms)| {
            let (t, _) = deeper.iter().filter(|&&(_, trip)| trip <= within_us).max_by_key(|&&(_, trip)| trip)?;
            Some(Transition { to: *t as u8, after_ms })
        });
    }
    out
}

/// The feature's 256-byte data structure for `plan` (Figure 393): each entry
/// ITPT in bits 31:08 and ITPS in bits 07:03, little-endian, unused entries
/// zero.
pub fn encode(plan: &[Option<Transition>; STATES]) -> [u8; STATES * 8] {
    let mut out = [0u8; STATES * 8];
    for (n, entry) in plan.iter().enumerate() {
        if let Some(t) = entry {
            let word = (t.after_ms << 8) | (u64::from(t.to) << 3);
            out[n * 8..n * 8 + 8].copy_from_slice(&word.to_le_bytes());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A descriptor's bytes from its fields.
    fn descriptor(mp_centiwatts: u16, nops: bool, entry_us: u32, exit_us: u32, idle_centiwatts: u16) -> [u8; 32] {
        let mut d = [0u8; 32];
        d[0..2].copy_from_slice(&mp_centiwatts.to_le_bytes());
        d[3] = if nops { 0b10 } else { 0 };
        d[4..8].copy_from_slice(&entry_us.to_le_bytes());
        d[8..12].copy_from_slice(&exit_us.to_le_bytes());
        d[16..18].copy_from_slice(&idle_centiwatts.to_le_bytes());
        if idle_centiwatts != 0 {
            d[18] = 0b10 << 6;
        }
        d
    }

    fn identify(descriptors: &[[u8; 32]], apsta: bool) -> [u8; 4096] {
        let mut id = [0u8; 4096];
        id[NPSS] = descriptors.len() as u8 - 1;
        id[APSTA] = apsta as u8;
        for (n, d) in descriptors.iter().enumerate() {
            id[DESCRIPTORS + n * 32..DESCRIPTORS + n * 32 + 32].copy_from_slice(d);
        }
        id
    }

    /// A five-state client drive: three operational, two non-operational.
    fn client() -> [[u8; 32]; 5] {
        [
            descriptor(650, false, 0, 0, 0),
            descriptor(450, false, 0, 0, 0),
            descriptor(350, false, 0, 0, 0),
            descriptor(4, true, 1_000, 2_000, 0),
            descriptor(1, true, 5_000, 40_000, 0),
        ]
    }

    #[test]
    fn a_descriptor_reads_as_figure_313_lays_it_out() {
        let mut d = descriptor(650, true, 1_500, 3_000, 5);
        let s = PowerState::parse(&d);
        assert_eq!(s, PowerState { non_operational: true, entry_us: 1_500, exit_us: 3_000, idle_power: Some(500), max_power: Some(65_000) });
        // MXPS set: the maximum power is already in 0.0001 W.
        d[3] |= 1;
        assert_eq!(PowerState::parse(&d).max_power, Some(650));
        // IPS 01b: the idle power is in 0.0001 W; 11b is reserved and reads as unreported.
        d[18] = 0b01 << 6;
        assert_eq!(PowerState::parse(&d).idle_power, Some(5));
        d[18] = 0b11 << 6;
        assert_eq!(PowerState::parse(&d).idle_power, None);
    }

    #[test]
    fn npss_is_zero_based_and_apsta_is_bit_0() {
        let (apst, read) = states(&identify(&client(), true)).unwrap();
        assert!(apst);
        assert_eq!(read.len(), 5);
        assert!(!states(&identify(&client(), false)).unwrap().0);
        assert!(states(&[0u8; 2048]).is_none());
    }

    #[test]
    fn a_state_past_the_first_tier_is_reached_from_the_first_tiers_after_the_second_tiers_time() {
        let (_, s) = states(&identify(&client(), true)).unwrap();
        let plan = plan(&s);
        let three = Some(Transition { to: 3, after_ms: 100 });
        assert_eq!(plan[..3], [three, three, three]);
        assert_eq!(plan[3], Some(Transition { to: 4, after_ms: 2_000 }));
        assert_eq!(plan[4], None);
        assert!(plan[5..].iter().all(Option::is_none));
    }

    /// The mutation the track names: a state past the bound is never a target.
    #[test]
    fn a_state_whose_round_trip_exceeds_the_bound_is_never_a_target() {
        let mut d = client();
        d[4] = descriptor(1, true, 10_000, 90_001, 0);
        let (_, s) = states(&identify(&d, true)).unwrap();
        let plan = plan(&s);
        assert!(plan.iter().flatten().all(|t| t.to != 4));
        assert_eq!(plan[3], None);
        // Exactly the bound is within it.
        d[4] = descriptor(1, true, 10_000, 90_000, 0);
        let (_, s) = states(&identify(&d, true)).unwrap();
        assert_eq!(super::plan(&s)[3], Some(Transition { to: 4, after_ms: 2_000 }));
    }

    #[test]
    fn a_state_with_an_unreported_latency_is_never_a_target() {
        let mut d = client();
        d[3] = descriptor(4, true, 0, 2_000, 0);
        d[4] = descriptor(1, true, 5_000, 0, 0);
        let (_, s) = states(&identify(&d, true)).unwrap();
        assert!(plan(&s).iter().all(Option::is_none));
    }

    #[test]
    fn an_operational_state_is_never_a_target() {
        let d = [descriptor(650, false, 0, 0, 0), descriptor(450, false, 100, 100, 0)];
        let (_, s) = states(&identify(&d, true)).unwrap();
        assert!(plan(&s).iter().all(Option::is_none));
    }

    #[test]
    fn no_target_reports_more_idle_power_than_the_state_it_leaves() {
        let d = [
            descriptor(650, false, 0, 0, 30),
            descriptor(4, true, 1_000, 2_000, 50),
            descriptor(1, true, 5_000, 10_000, 1),
        ];
        let (_, s) = states(&identify(&d, true)).unwrap();
        let plan = plan(&s);
        assert_eq!(plan[0], Some(Transition { to: 2, after_ms: 100 }));
        assert_eq!(plan[1], Some(Transition { to: 2, after_ms: 100 }));
    }

    #[test]
    fn the_table_encodes_itpt_in_bits_31_8_and_itps_in_7_3() {
        let (_, s) = states(&identify(&client(), true)).unwrap();
        let table = encode(&plan(&s));
        assert_eq!(u64::from_le_bytes(table[0..8].try_into().unwrap()), (100 << 8) | (3 << 3));
        assert_eq!(u64::from_le_bytes(table[24..32].try_into().unwrap()), (2_000 << 8) | (4 << 3));
        assert!(table[32..].iter().all(|&b| b == 0));
    }

    /// The T14's SK hynix BC501, as its descriptors state it, gets the table
    /// Linux set on it (evidence `pr-1131` r5): every state straight to PS4
    /// after 100 ms, past PS3, both within the first tier.
    #[test]
    fn the_deepest_state_within_a_tier_is_the_target_past_shallower_ones() {
        let d = [
            descriptor(350, false, 5, 5, 0),
            descriptor(240, false, 30, 30, 0),
            descriptor(190, false, 100, 100, 0),
            descriptor(4, true, 1_000, 1_000, 0),
            descriptor(1, true, 1_000, 5_000, 0),
        ];
        let (_, s) = states(&identify(&d, true)).unwrap();
        let four = Some(Transition { to: 4, after_ms: 100 });
        assert_eq!(plan(&s)[..5], [four, four, four, four, None]);
    }
}
