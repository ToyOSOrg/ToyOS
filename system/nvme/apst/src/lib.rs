//! The Autonomous Power State Transition table for one NVMe controller, built
//! from its own power state descriptors (NVM Express Base Specification 2.1:
//! Identify Controller, Figure 312; the descriptor, Figure 313; the feature,
//! §5.1.25.1.6, Figures 392 and 393; power management, §8.1.17).
//!
//! **A target is a non-operational state whose stated round trip, its entry
//! plus its exit latency, is within [`ROUND_TRIP_BOUND_US`].** A state that
//! does not report both (`0h` is "not reported") states no round trip and is
//! never a target. The bound is on the target's own latencies: a transition
//! also costs the exit latency of the state it leaves (§8.1.17).
//!
//! **Each state goes to the highest-numbered target after it within the first
//! [`TIERS`] tier that holds one**, reporting no higher idle power, after that
//! tier's idle time: a round trip within 15 ms after 100 ms idle, one within
//! 100 ms after 2 s. States are ranked by number because the specification
//! numbers them so that none consumes more maximum power than the one before
//! it (§8.1.17); their latencies carry no order. The tiers are policy: a state
//! cheap to leave is entered soon, and a dear one only once the drive has been
//! idle twenty times longer than leaving it costs. A drive's table is taken as
//! it states it; there is no table of drives.

#![forbid(unsafe_code)]

/// Identify Controller's length, and the bytes this reads of it: NPSS at 263,
/// APSTA at 265, and up to 32 power state descriptors from 2048, 32 bytes each
/// (Figure 312).
pub const IDENTIFY_BYTES: usize = 4096;
pub const NPSS: usize = 263;
pub const APSTA: usize = 265;
pub const DESCRIPTORS: usize = 2048;
pub const DESCRIPTOR_BYTES: usize = 32;
pub const STATES: usize = 32;

const _: () = assert!(DESCRIPTORS + STATES * DESCRIPTOR_BYTES <= IDENTIFY_BYTES);

/// The most round trip a target may state, in microseconds: under a tenth of
/// a second, below what a person notices as a pause. The specification bounds
/// no latency; this is policy.
pub const ROUND_TRIP_BOUND_US: u64 = 100_000;

/// The tiers, shallowest first: the most round trip a target may state, in
/// microseconds, and the idle time before the transition, in milliseconds.
/// The last tier's round trip is [`ROUND_TRIP_BOUND_US`].
pub const TIERS: [(u64, u64); 2] = [(15_000, 100), (ROUND_TRIP_BOUND_US, 2_000)];

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
}

/// Identify Controller's NPSS, a zero-based count, names more power states
/// than the [`STATES`] it holds descriptors for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TooManyStates(pub u8);

/// The controller's power states from its Identify Controller data, in state
/// order, and whether it supports APST.
pub fn states(identify: &[u8; IDENTIFY_BYTES]) -> Result<(bool, Vec<PowerState>), TooManyStates> {
    let npss = identify[NPSS];
    let count = usize::from(npss) + 1;
    if count > STATES {
        return Err(TooManyStates(npss));
    }
    let descriptors = &identify[DESCRIPTORS..DESCRIPTORS + count * DESCRIPTOR_BYTES];
    let states = descriptors.as_chunks::<DESCRIPTOR_BYTES>().0.iter().map(PowerState::parse).collect();
    Ok((identify[APSTA] & 1 != 0, states))
}

/// One entry of the table: the state to move to and the idle time before
/// it, or `None` for no transition out of this state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transition {
    pub to: u8,
    pub after_ms: u64,
}

/// Each state's transition, in state order, for `states` as [`states`] reads
/// them: at most [`STATES`].
pub fn plan(states: &[PowerState]) -> Vec<Option<Transition>> {
    assert!(states.len() <= STATES, "a controller has at most {STATES} power states");
    states
        .iter()
        .enumerate()
        .map(|(n, from)| {
            let targets: Vec<(usize, u64)> = states
                .iter()
                .enumerate()
                .skip(n + 1)
                .filter(|(_, to)| to.non_operational)
                .filter(|(_, to)| match (to.idle_power, from.idle_power) {
                    (Some(to), Some(from)) => to <= from,
                    _ => true,
                })
                .filter_map(|(t, to)| Some((t, to.round_trip_us()?)))
                .collect();
            TIERS.iter().find_map(|&(within_us, after_ms)| {
                let &(to, _) = targets.iter().rev().find(|&&(_, trip)| trip <= within_us)?;
                Some(Transition { to: to as u8, after_ms })
            })
        })
        .collect()
}

/// The feature's 256-byte data structure for `plan` (Figure 393): each entry
/// ITPT in bits 31:08 and ITPS in bits 07:03, little-endian, unused entries
/// zero.
pub fn encode(plan: &[Option<Transition>]) -> [u8; STATES * 8] {
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

    /// `d` with its maximum power in 0.0001 W, MXPS set.
    fn fine(mut d: [u8; 32], max_tenth_milliwatts: u16) -> [u8; 32] {
        d[0..2].copy_from_slice(&max_tenth_milliwatts.to_le_bytes());
        d[3] |= 1;
        d
    }

    fn identify(descriptors: &[[u8; 32]], apsta: bool) -> [u8; IDENTIFY_BYTES] {
        let mut id = [0u8; IDENTIFY_BYTES];
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
    }

    /// Identify Controller holds 32 descriptors, so NPSS 31 is the most there
    /// is; a controller saying more is refused, not read past.
    #[test]
    fn an_npss_past_31_is_refused_and_31_reads_all_32_states() {
        let mut id = identify(&client(), true);
        id[DESCRIPTORS + 31 * 32..DESCRIPTORS + 32 * 32].copy_from_slice(&descriptor(1, true, 1_000, 1_000, 0));
        id[NPSS] = 31;
        let (_, s) = states(&id).unwrap();
        assert_eq!(s.len(), STATES);
        // The last state is a target, and its number fills ITPS's five bits.
        let plan = plan(&s);
        assert_eq!(plan[0], Some(Transition { to: 31, after_ms: 100 }));
        assert_eq!(plan[31], None);
        assert_eq!(u64::from_le_bytes(encode(&plan)[0..8].try_into().unwrap()), (100 << 8) | (31 << 3));
        for npss in [32, 255] {
            id[NPSS] = npss;
            assert_eq!(states(&id), Err(TooManyStates(npss)));
        }
    }

    #[test]
    fn a_state_past_the_first_tier_is_reached_from_the_first_tiers_after_the_second_tiers_time() {
        let (_, s) = states(&identify(&client(), true)).unwrap();
        let three = Some(Transition { to: 3, after_ms: 100 });
        assert_eq!(plan(&s), [three, three, three, Some(Transition { to: 4, after_ms: 2_000 }), None]);
    }

    /// The specification numbers states by maximum power, not by latency, and
    /// permits a later state to be quicker to leave: here PS2 draws less than
    /// PS1 and leaves faster. Every state goes to PS2, and none goes back.
    #[test]
    fn the_target_is_the_highest_numbered_whatever_the_latencies() {
        let d = [
            descriptor(650, false, 0, 0, 0),
            descriptor(4, true, 5_000, 5_000, 0),
            descriptor(1, true, 1_000, 1_000, 0),
        ];
        let (_, s) = states(&identify(&d, true)).unwrap();
        let two = Some(Transition { to: 2, after_ms: 100 });
        assert_eq!(plan(&s), [two, two, None]);
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

    /// The T14's SK hynix BC501, as the driver logs its descriptors: every
    /// state goes straight to PS4 after 100 ms, past PS3, both within the
    /// first tier. The table Linux sets on it too.
    #[test]
    fn the_t14s_drive_gets_the_table_linux_sets() {
        let d = [
            descriptor(350, false, 5, 5, 0),
            descriptor(240, false, 30, 30, 0),
            descriptor(190, false, 100, 100, 0),
            fine(descriptor(0, true, 1_000, 1_000, 0), 350),
            fine(descriptor(0, true, 1_000, 5_000, 0), 50),
        ];
        let (_, s) = states(&identify(&d, true)).unwrap();
        let four = Some(Transition { to: 4, after_ms: 100 });
        assert_eq!(plan(&s), [four, four, four, four, None]);
    }

    /// The AMD bench's drive, as the driver logs its descriptors: PS3 a
    /// 1.41 ms round trip and PS4 10 ms, both within the first tier, so every
    /// state goes to PS4 after 100 ms. The table Linux sets on it too.
    #[test]
    fn the_amd_benchs_drive_gets_the_table_linux_sets() {
        let d = [
            descriptor(800, false, 0, 0, 0),
            descriptor(630, false, 0, 0, 0),
            descriptor(350, false, 0, 0, 0),
            fine(descriptor(0, true, 210, 1_200, 0), 760),
            fine(descriptor(0, true, 2_000, 8_000, 0), 50),
        ];
        let (_, s) = states(&identify(&d, true)).unwrap();
        let four = Some(Transition { to: 4, after_ms: 100 });
        assert_eq!(plan(&s), [four, four, four, four, None]);
    }
}
