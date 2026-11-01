//! The congestion window: HyStart++ in the initial slow start (RFC 9406, as RFC 9438 §4.10
//! recommends) and RFC 5681 §3.1's slow start after it, CUBIC in avoidance (RFC 9438), and the
//! reductions a loss or a timeout makes. The window grows only while [`Cc::limited`] says it is in
//! use, and CUBIC's clock skips the time it was not (§4.2). The cube is computed in fixed point: no
//! float, no `libm`.

use core::time::Duration;

use crate::seq::Seq;
use crate::Instant;

/// RFC 5681 §3.1: "arbitrarily high", the largest window a scaled peer can offer.
pub const SSTHRESH_INITIAL: u32 = 1 << 30;
/// Fixed-point fraction bits of the estimate and the growth carry.
const FRACTION: u32 = 16;
/// `C = 0.4` segments per second cubed (RFC 9438 §5.1), with time in microseconds:
/// `W(t) = W_max + (2 · SMSS · t³) / (5 · 10^18)`.
const CUBE_DENOMINATOR: i128 = 5_000_000_000_000_000_000;
/// The farthest from `K` the cubic is evaluated, so the cube stays inside `i128`.
const CUBE_SPAN_US: i128 = 1 << 34;

/// RFC 9406 §4.3's constants; L is infinity, as for a paced sender (`pace`).
const MIN_RTT_THRESH: Duration = Duration::from_millis(4);
const MAX_RTT_THRESH: Duration = Duration::from_millis(16);
const MIN_RTT_DIVISOR: u32 = 8;
const N_RTT_SAMPLE: u32 = 8;
const CSS_GROWTH_DIVISOR: u32 = 4;
const CSS_ROUNDS: u32 = 5;

/// RFC 6928 §2.
pub fn initial_window(smss: u32) -> u32 {
    smss.saturating_mul(10).min(smss.saturating_mul(2).max(14_600))
}

fn us(d: Duration) -> u64 {
    u64::try_from(d.as_micros()).unwrap_or(u64::MAX)
}

/// `⌊x^(1/3)⌋`.
fn cube_root(x: u128) -> u64 {
    let (mut low, mut high) = (0u64, 1u64 << 43);
    while low < high {
        let mid = low.saturating_add(high.saturating_sub(low).div_ceil(2));
        let m = u128::from(mid);
        if m.saturating_mul(m).saturating_mul(m) <= x {
            low = mid;
        } else {
            high = mid.saturating_sub(1);
        }
    }
    low
}

/// An ACK outside loss recovery: `bytes` newly acknowledged, SND.UNA after it and SND.NXT, and
/// the RTT sample it gave, if any.
#[derive(Clone, Copy, Debug)]
pub struct Ack {
    pub now: Instant,
    pub bytes: u32,
    pub una: Seq,
    pub nxt: Seq,
    pub rtt: Option<Duration>,
    pub srtt: Option<Duration>,
}

/// A HyStart++ transition an ACK made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlowStart {
    /// A round's minimum RTT rose past the threshold: Conservative Slow Start. The check runs on
    /// every ACK of a round once it has its samples (RFC 9406 §4.2), so a round can enter more than
    /// once; `first` is its first entry.
    Conservative { first: bool },
    /// A CSS round's minimum fell below its baseline: the exit was spurious.
    Resumed,
    /// CSS ran its rounds: ssthresh is cwnd, and avoidance follows.
    Ended,
}

/// RFC 9406 §4.2's round and delay state. `None` is the RFC's infinity.
#[derive(Clone, Copy, Debug, Default)]
struct HyStart {
    /// windowEnd: the round ends when it is acknowledged.
    window_end: Option<Seq>,
    last_round_min: Option<Duration>,
    current_round_min: Option<Duration>,
    samples: u32,
    css: Option<Css>,
    css_this_round: bool,
}

/// Conservative Slow Start: the baseline it compares against and the rounds it has begun.
#[derive(Clone, Copy, Debug)]
struct Css {
    baseline: Duration,
    rounds: u32,
}

/// RttThresh: how far a round's minimum RTT must rise over the last round's to leave slow start.
fn rtt_thresh(last_round_min: Duration) -> Duration {
    last_round_min.checked_div(MIN_RTT_DIVISOR).unwrap_or_default().clamp(MIN_RTT_THRESH, MAX_RTT_THRESH)
}

impl HyStart {
    /// What the ACK grows cwnd by when the window was in use, of `bytes` newly acknowledged.
    fn growth(&self, bytes: u32) -> u32 {
        if self.css.is_some() { bytes / CSS_GROWTH_DIVISOR } else { bytes }
    }

    /// The ACK's RTT sample and round, after its growth.
    fn observe(&mut self, ack: &Ack) -> Option<SlowStart> {
        if let Some(rtt) = ack.rtt {
            self.current_round_min = Some(self.current_round_min.map_or(rtt, |min| min.min(rtt)));
            self.samples = self.samples.saturating_add(1);
        }
        let mut change = None;
        if let (true, Some(current)) = (self.samples >= N_RTT_SAMPLE, self.current_round_min) {
            match self.css {
                None => {
                    if let Some(last) = self.last_round_min {
                        if current >= last.saturating_add(rtt_thresh(last)) {
                            // The round the exit happens in is CSS's first.
                            self.css = Some(Css { baseline: current, rounds: 1 });
                            change = Some(SlowStart::Conservative { first: !core::mem::replace(&mut self.css_this_round, true) });
                        }
                    }
                }
                Some(css) if current < css.baseline => {
                    self.css = None;
                    change = Some(SlowStart::Resumed);
                }
                Some(_) => {}
            }
        }
        match self.window_end {
            Some(end) if ack.una.at_or_after(end) => {}
            Some(_) => return change,
            None => {
                self.window_end = Some(ack.nxt);
                return change;
            }
        }
        self.window_end = Some(ack.nxt);
        self.last_round_min = self.current_round_min.take();
        self.samples = 0;
        self.css_this_round = false;
        match self.css.as_mut() {
            Some(css) if css.rounds >= CSS_ROUNDS => Some(SlowStart::Ended),
            Some(css) => {
                css.rounds = css.rounds.saturating_add(1);
                change
            }
            None => change,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Epoch {
    start: Instant,
    k_us: u64,
    w_max: u32,
    estimate: u64,
    carry: u64,
}

impl Epoch {
    fn cubic(&self, smss: u32, t_us: u64) -> u64 {
        let d = i128::from(t_us).saturating_sub(i128::from(self.k_us)).clamp(-CUBE_SPAN_US, CUBE_SPAN_US);
        let cube = d.saturating_mul(d).saturating_mul(d).saturating_mul(i128::from(smss).saturating_mul(2));
        let w = i128::from(self.w_max).saturating_add(cube.checked_div(CUBE_DENOMINATOR).unwrap_or(0));
        u64::try_from(w.max(0)).unwrap_or(u64::MAX)
    }
}

#[derive(Clone, Debug)]
pub struct Cc {
    pub cwnd: u32,
    pub ssthresh: u32,
    pub smss: u32,
    /// The window is in use: cwnd stopped the sender's last transmit opportunity with data
    /// waiting, or the caller's credit or the pacer ended it with at least half of cwnd in flight
    /// (`hand_off`). Every ACK until the next opportunity grows the window, however many one pass
    /// brings.
    pub limited: bool,
    w_max: Option<u32>,
    /// RFC 9438 §4.3's cwnd_prior: cwnd when ssthresh was last set, and before that the initial
    /// cwnd, which avoidance before any congestion event is past.
    prior: u32,
    after_timeout: bool,
    epoch: Option<Epoch>,
    last_ack: Option<Instant>,
    /// Runs while ssthresh is still [`SSTHRESH_INITIAL`] (RFC 9406 §4.3).
    hystart: HyStart,
}

impl Cc {
    pub fn new(smss: u32, handshake_retransmissions: u32) -> Self {
        let cwnd = if handshake_retransmissions > 1 { smss } else { initial_window(smss) };
        Self {
            cwnd,
            ssthresh: SSTHRESH_INITIAL,
            smss,
            limited: false,
            w_max: None,
            prior: cwnd,
            after_timeout: false,
            epoch: None,
            last_ack: None,
            hystart: HyStart::default(),
        }
    }

    pub fn on_ack(&mut self, ack: &Ack) -> Option<SlowStart> {
        let Ack { now, bytes: acked, srtt, .. } = *ack;
        let grow = self.limited;
        let since_last = self.last_ack.replace(now).map_or(Duration::ZERO, |last| now.since(last));
        if self.ssthresh == SSTHRESH_INITIAL {
            if grow {
                self.cwnd = self.cwnd.saturating_add(self.hystart.growth(acked));
            }
            let change = self.hystart.observe(ack);
            if change == Some(SlowStart::Ended) {
                // RFC 9438 §4.10: avoidance starts with K = 0 and W_max = cwnd_prior = cwnd.
                self.ssthresh = self.cwnd;
                self.prior = self.cwnd;
            }
            return change;
        }
        if self.cwnd < self.ssthresh {
            if grow {
                self.cwnd = self.cwnd.saturating_add(acked.min(self.smss)).min(self.ssthresh);
            }
            return None;
        }
        let smss = self.smss;
        let cwnd = self.cwnd;
        let epoch = self.epoch.get_or_insert_with(|| {
            let (k_us, w_max) = match self.w_max {
                Some(w_max) if w_max > cwnd && !self.after_timeout => {
                    let gap = u128::from(w_max.saturating_sub(cwnd)).saturating_mul(2_500_000_000_000_000_000);
                    (cube_root(gap.checked_div(u128::from(smss)).unwrap_or(0)), w_max)
                }
                _ => (0, cwnd),
            };
            Epoch { start: now, k_us, w_max, estimate: u64::from(cwnd) << FRACTION, carry: 0 }
        });
        self.w_max = Some(epoch.w_max);
        self.after_timeout = false;
        if !grow {
            epoch.start = epoch.start.after(since_last).min(now);
            return None;
        }
        let t = us(now.since(epoch.start));
        let w_t = epoch.cubic(smss, t);
        let cwnd64 = u64::from(cwnd);
        let target = epoch.cubic(smss, t.saturating_add(srtt.map_or(0, us))).clamp(cwnd64, cwnd64.saturating_mul(3) / 2);
        let step = (u64::from(acked).saturating_mul(u64::from(smss)) << FRACTION).checked_div(cwnd64).unwrap_or(0);
        let reno_friendly = epoch.estimate >> FRACTION >= u64::from(self.prior);
        epoch.estimate = epoch.estimate.saturating_add(if reno_friendly { step } else { step.saturating_mul(9) / 17 });
        let cwnd = if w_t < epoch.estimate >> FRACTION {
            epoch.estimate >> FRACTION
        } else {
            let gain = (target.saturating_sub(cwnd64) << FRACTION).saturating_mul(u64::from(acked));
            epoch.carry = epoch.carry.saturating_add(gain.checked_div(cwnd64).unwrap_or(0));
            let whole = epoch.carry >> FRACTION;
            epoch.carry &= (1 << FRACTION) - 1;
            cwnd64.saturating_add(whole)
        };
        self.cwnd = u32::try_from(cwnd).unwrap_or(u32::MAX);
        None
    }

    /// Entering fast or SACK recovery (RFC 9438 §4.6, §4.7). `flight` excludes Limited Transmit.
    pub fn on_loss(&mut self, flight: u32) {
        self.epoch = None;
        self.w_max = Some(match self.w_max {
            Some(w_max) if self.cwnd < w_max => u32::try_from(u64::from(self.cwnd).saturating_mul(17) / 20).unwrap_or(u32::MAX),
            _ => self.cwnd,
        });
        self.prior = self.cwnd;
        self.ssthresh = self.reduced(flight);
    }

    /// A retransmission timeout (RFC 5681 §3.1, RFC 9438 §4.8). `repeat` is a second timeout of
    /// the same segment, which sets neither ssthresh nor cwnd_prior again.
    pub fn on_timeout(&mut self, flight: u32, repeat: bool) {
        if !repeat {
            self.ssthresh = self.reduced(flight);
            self.prior = self.cwnd;
        }
        self.cwnd = self.smss;
        self.epoch = None;
        self.after_timeout = true;
    }

    fn reduced(&self, flight: u32) -> u32 {
        u32::try_from(u64::from(flight).saturating_mul(7) / 10).unwrap_or(u32::MAX).max(self.smss.saturating_mul(2))
    }

    /// Recovery ended: the next ACK in avoidance starts an epoch.
    pub fn end_recovery(&mut self) {
        self.epoch = None;
    }

    /// Before new data after an idle longer than the RTO (RFC 5681 §4.1, RFC 6928 §2).
    pub fn restart_after_idle(&mut self) {
        self.cwnd = self.cwnd.min(initial_window(self.smss));
    }

    /// A smaller effective MSS (RFC 1191 §6.4); during the initial slow start cwnd keeps its
    /// segment count (RFC 5681 §3.1).
    pub fn set_smss(&mut self, smss: u32) {
        if self.ssthresh == SSTHRESH_INITIAL {
            let scaled = u64::from(self.cwnd).saturating_mul(u64::from(smss)).checked_div(u64::from(self.smss)).unwrap_or(0);
            self.cwnd = u32::try_from(scaled).unwrap_or(u32::MAX);
        }
        self.smss = smss;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: u64) -> Instant {
        Instant::from_millis(ms)
    }

    fn ack(now: Instant, bytes: u32, srtt: Option<Duration>) -> Ack {
        Ack { now, bytes, una: Seq::new(0), nxt: Seq::new(0), rtt: None, srtt }
    }

    fn avoidance(cwnd: u32, w_max: u32) -> Cc {
        let mut cc = Cc::new(1460, 0);
        cc.cwnd = cwnd;
        cc.ssthresh = cwnd;
        cc.w_max = Some(w_max);
        cc
    }

    #[test]
    fn s_cc_007_slow_start_stops_at_ssthresh() {
        let mut cc = Cc::new(1460, 0);
        cc.cwnd = 13_140;
        cc.ssthresh = 14_000;
        cc.limited = true;
        cc.on_ack(&ack(at(0), 2920, None));
        assert_eq!(cc.cwnd, 14_000);
        assert!(cc.epoch.is_none());
        cc.limited = true;
        cc.on_ack(&ack(at(10), 1460, None));
        assert!(cc.epoch.is_some(), "the next ACK is avoidance");
    }

    #[test]
    fn s_cc_008_the_epoch_after_a_timeout() {
        let mut cc = Cc::new(1460, 0);
        cc.on_timeout(14_600, false);
        assert_eq!((cc.ssthresh, cc.cwnd), (10_220, 1460));
        let mut t = 0;
        while cc.cwnd < cc.ssthresh {
            t += 1;
            cc.limited = true;
            cc.on_ack(&ack(at(t), 1460, None));
        }
        assert_eq!(cc.cwnd, 10_220);
        cc.limited = true;
        cc.on_ack(&ack(at(t + 1), 1460, None));
        let epoch = cc.epoch.unwrap();
        assert_eq!((epoch.k_us, epoch.w_max), (0, 10_220));
    }

    #[test]
    fn s_cc_010_congestion_events() {
        let mut cc = Cc::new(1460, 0);
        cc.cwnd = 146_000;
        cc.on_loss(146_000);
        assert_eq!((cc.ssthresh, cc.w_max, cc.prior), (102_200, Some(146_000), 146_000));
        cc.cwnd = 130_000;
        cc.on_loss(130_000);
        assert_eq!(cc.w_max, Some(110_500), "fast convergence");
    }

    /// A second timeout of the same segment sets neither ssthresh nor cwnd_prior (RFC 9438 §4.3).
    #[test]
    fn s_cc_014_flight_not_cwnd_at_a_timeout() {
        let mut cc = Cc::new(1460, 0);
        cc.cwnd = 29_200;
        cc.on_timeout(14_600, false);
        assert_eq!((cc.ssthresh, cc.cwnd, cc.prior), (10_220, 1460, 29_200));
        cc.on_timeout(1460, true);
        assert_eq!((cc.ssthresh, cc.cwnd, cc.prior), (10_220, 1460, 29_200));
    }

    #[test]
    fn s_cc_009_cubic_values() {
        let mut cc = avoidance(102_200, 146_000);
        cc.prior = 146_000;
        cc.on_ack(&ack(at(0), 0, None));
        let epoch = cc.epoch.unwrap();
        assert!(epoch.k_us.abs_diff(4_217_200) <= 1_000, "K = {} µs", epoch.k_us);
        for (t, want) in [(0u64, 102_200f64), (1_000_000, 126_554.0), (2_000_000, 139_635.0), (epoch.k_us, 146_000.0), (5_000_000, 146_280.0)] {
            let got = epoch.cubic(1460, t) as f64;
            assert!((got - want).abs() <= want * 0.005, "W({t}) = {got}, want {want}");
        }
    }

    /// CC-11's epoch (K = 0, W_max = cwnd = 14,600) as production reaches it: the first avoidance
    /// after a timeout that set ssthresh 14,600 from a cwnd of 21,900, so W_est stays below
    /// cwnd_prior and grows by alpha_cubic.
    #[test]
    fn s_cc_011_reno_friendly_region() {
        let mut cc = Cc::new(1460, 0);
        let mut t = 0;
        let segment = |cc: &mut Cc, t: &mut u64, step: u64, srtt: Option<Duration>| {
            *t += step;
            cc.limited = true;
            cc.on_ack(&ack(at(*t), 1460, srtt));
        };
        while cc.cwnd < 21_900 {
            segment(&mut cc, &mut t, 1, None);
        }
        cc.on_timeout(20_858, false);
        assert_eq!((cc.ssthresh, cc.prior), (14_600, 21_900));
        while cc.cwnd < cc.ssthresh {
            segment(&mut cc, &mut t, 1, None);
        }
        for _ in 0..10 {
            segment(&mut cc, &mut t, 5, Some(Duration::from_millis(5)));
        }
        let epoch = cc.epoch.unwrap();
        assert_eq!((epoch.k_us, epoch.w_max), (0, 14_600));
        assert!((f64::from(cc.cwnd) - 15_355.0).abs() <= 153.0, "cwnd {}", cc.cwnd);
    }

    #[test]
    fn s_cc_013_application_limited_time_is_skipped() {
        let mut cc = avoidance(102_200, 146_000);
        cc.on_ack(&ack(at(0), 0, None));
        for ms in (100..=2_000).step_by(100) {
            cc.on_ack(&ack(at(ms), 1460, None));
        }
        let start = cc.epoch.unwrap().start;
        cc.limited = true;
        cc.on_ack(&ack(at(3_000), 1460, None));
        assert_eq!(at(3_000).since(start), Duration::from_secs(1));
    }

    #[test]
    fn s_cc_015_prop_avoidance_growth_is_bounded() {
        let mut state = 0x243f_6a88_85a3_08d3u64;
        let mut next = move |bound: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % bound
        };
        for _ in 0..200 {
            let cwnd = 2_920 + u32::try_from(next(200_000)).unwrap();
            let w_max = cwnd + u32::try_from(next(100_000)).unwrap();
            let mut cc = avoidance(cwnd, w_max);
            let srtt = Duration::from_millis(1 + next(200));
            let mut now = 0;
            cc.on_ack(&ack(at(now), 0, Some(srtt)));
            for _ in 0..12 {
                let round_start = cc.cwnd;
                let mut acked = 0u32;
                while acked < round_start {
                    let n = 1 + u32::try_from(next(2 * 1460)).unwrap();
                    now += next(u64::try_from(srtt.as_millis()).unwrap() + 1);
                    let before = cc.cwnd;
                    let epoch = cc.epoch.unwrap();
                    let t = us(at(now).since(epoch.start));
                    let target = epoch.cubic(1460, t + us(srtt)).clamp(u64::from(before), u64::from(before) * 3 / 2);
                    cc.limited = next(4) != 0;
                    cc.on_ack(&ack(at(now), n, Some(srtt)));
                    let reno = cc.epoch.unwrap().estimate >> FRACTION;
                    assert!(u64::from(cc.cwnd) <= target.max(reno), "cwnd {} above target {target}", cc.cwnd);
                    acked += n;
                }
                assert!(u64::from(cc.cwnd) * 2 <= u64::from(round_start) * 3 + 2 * 1460, "{} after {round_start}", cc.cwnd);
            }
        }
    }

    /// A sender keeping eight segments in flight, each ACK one segment with its RTT: after the
    /// first ACK, which starts the first round, every eighth ACK ends one.
    struct Rounds {
        cc: Cc,
        una: u32,
        t: u64,
        flight: u32,
    }

    impl Rounds {
        fn new() -> Self {
            let mut r = Self { cc: Cc::new(1460, 0), una: 0, t: 0, flight: 8 };
            assert_eq!(r.ack(Some(10)), None);
            r
        }

        fn ack(&mut self, rtt_ms: Option<u64>) -> Option<SlowStart> {
            self.una += 1460;
            self.t += 1;
            self.cc.limited = true;
            let rtt = rtt_ms.map(Duration::from_millis);
            self.cc.on_ack(&Ack { now: at(self.t), bytes: 1460, una: Seq::new(self.una), nxt: Seq::new(self.una + self.flight * 1460), rtt, srtt: rtt })
        }

        /// A round of eight ACKs at `rtt_ms`: the transitions it made, by ACK.
        fn round(&mut self, rtt_ms: u64) -> Vec<(usize, SlowStart)> {
            self.acks(&[rtt_ms; 8])
        }

        fn acks(&mut self, rtts_ms: &[u64]) -> Vec<(usize, SlowStart)> {
            rtts_ms.iter().enumerate().filter_map(|(i, &rtt)| self.ack(Some(rtt)).map(|c| (i, c))).collect()
        }
    }

    #[test]
    fn s_cc_019_rtt_thresh_is_an_eighth_clamped_to_4_and_16_ms() {
        let ms = Duration::from_millis;
        assert_eq!(rtt_thresh(ms(10)), ms(4));
        assert_eq!(rtt_thresh(ms(64)), ms(8));
        assert_eq!(rtt_thresh(ms(200)), ms(16));
    }

    /// RFC 9406 §4.2: the eighth sample of a round whose minimum rose by RttThresh starts CSS,
    /// whose growth is a quarter; CSS's fifth round, counting the one it began in, ends slow start
    /// with ssthresh at cwnd, and avoidance starts from it (RFC 9438 §4.10).
    #[test]
    fn s_cc_020_a_rising_round_trip_ends_slow_start_after_css() {
        let mut r = Rounds::new();
        assert_eq!(r.round(10), []);
        assert_eq!(r.round(13), [], "under 10 ms + 4 ms");
        for _ in 0..7 {
            assert_eq!(r.ack(Some(17)), None, "fewer than eight samples");
        }
        assert_eq!(r.ack(Some(17)), Some(SlowStart::Conservative { first: true }), "and the round ends: CSS's first");
        let before = r.cc.cwnd;
        assert_eq!(r.ack(Some(17)), None);
        assert_eq!(r.cc.cwnd - before, 365, "CSS grows by a quarter");
        for _ in 0..7 {
            assert_eq!(r.ack(Some(17)), None);
        }
        for round in 3..5 {
            assert_eq!(r.round(17), [], "CSS round {round}");
        }
        assert_eq!(r.cc.ssthresh, SSTHRESH_INITIAL);
        assert_eq!(r.round(17), [(7, SlowStart::Ended)]);
        assert_eq!((r.cc.ssthresh, r.cc.prior), (r.cc.cwnd, r.cc.cwnd));
        let cwnd = r.cc.cwnd;
        r.ack(Some(17));
        let epoch = r.cc.epoch.unwrap();
        assert_eq!((epoch.k_us, epoch.w_max), (0, cwnd));
    }

    /// RFC 9406 §4.2: a CSS round whose minimum falls under the baseline resumes slow start.
    #[test]
    fn s_cc_021_a_round_trip_back_under_the_baseline_resumes_slow_start() {
        let mut r = Rounds::new();
        assert_eq!(r.round(10), []);
        assert_eq!(r.round(20), [(7, SlowStart::Conservative { first: true })]);
        assert_eq!(r.round(19), [(7, SlowStart::Resumed)]);
        let before = r.cc.cwnd;
        r.ack(Some(19));
        assert_eq!(r.cc.cwnd - before, 1460);
        assert_eq!(r.cc.ssthresh, SSTHRESH_INITIAL);
    }

    /// RFC 9406 runs only in the initial slow start, where a paced sender grows by all an ACK
    /// acknowledged (L = infinity, §4.3): after a loss, slow start is RFC 5681 §3.1's, one SMSS per
    /// ACK at most.
    #[test]
    fn s_cc_022_l_applies_only_to_the_initial_slow_start() {
        let mut cc = Cc::new(1460, 0);
        cc.limited = true;
        cc.on_ack(&ack(at(0), 20_000, None));
        assert_eq!(cc.cwnd, 14_600 + 20_000);
        cc.on_timeout(cc.cwnd, false);
        cc.limited = true;
        cc.on_ack(&ack(at(1), 20_000, None));
        assert_eq!(cc.cwnd, 2 * 1460);
    }

    /// RFC 9406 §4.2's checks on every ACK: in a round whose minimum keeps falling while it stays
    /// past the threshold, each lower sample resumes slow start and the next re-enters CSS from it.
    /// One round, four entries, one of them the round's first; a later round's entry is its own first.
    #[test]
    fn s_cc_023_a_falling_minimum_flaps_within_a_round() {
        let mut r = Rounds::new();
        assert_eq!(r.round(2), []);
        r.flight = 16;
        assert_eq!(r.round(2), [], "the last round before the rise, at 2 ms");
        let entry = |first| SlowStart::Conservative { first };
        let flaps = r.acks(&[9, 9, 9, 9, 9, 9, 9, 9, 8, 8, 7, 7, 6, 6, 6, 6]);
        let want = [(7, entry(true)), (8, SlowStart::Resumed), (9, entry(false)), (10, SlowStart::Resumed), (11, entry(false)), (12, SlowStart::Resumed), (13, entry(false))];
        assert_eq!(flaps, want);
        assert_eq!(r.acks(&[5; 16]), [(7, SlowStart::Resumed)]);
        assert_eq!(r.acks(&[12; 16]), [(7, entry(true))], "the next round's first entry");
    }
}
