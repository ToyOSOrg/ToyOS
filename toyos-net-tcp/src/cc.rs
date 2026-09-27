//! The congestion window: slow start with appropriate byte counting (RFC 5681 §3.1), CUBIC in
//! avoidance (RFC 9438), and the reductions a loss or a timeout makes. Growth happens only when
//! the window, not the application or the peer, limited the last send (RFC 9438 §5.8), and CUBIC's
//! clock skips the time that was so limited (§4.2). The cube is computed in fixed point: no float,
//! no `libm`.

use core::time::Duration;

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
    /// A send was held by cwnd, with data waiting and the peer's window open, since the last growth.
    pub limited: bool,
    w_max: Option<u32>,
    prior: Option<u32>,
    after_timeout: bool,
    epoch: Option<Epoch>,
    last_ack: Option<Instant>,
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
            prior: None,
            after_timeout: false,
            epoch: None,
            last_ack: None,
        }
    }

    /// An ACK that newly acknowledges `acked` bytes outside loss recovery.
    pub fn on_ack(&mut self, now: Instant, acked: u32, srtt: Option<Duration>) {
        let grow = core::mem::replace(&mut self.limited, false);
        let since_last = self.last_ack.replace(now).map_or(Duration::ZERO, |last| now.since(last));
        if self.cwnd < self.ssthresh {
            if grow {
                self.cwnd = self.cwnd.saturating_add(acked.min(self.smss)).min(self.ssthresh);
            }
            return;
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
            return;
        }
        let t = us(now.since(epoch.start));
        let w_t = epoch.cubic(smss, t);
        let cwnd64 = u64::from(cwnd);
        let target = epoch.cubic(smss, t.saturating_add(srtt.map_or(0, us))).clamp(cwnd64, cwnd64.saturating_mul(3) / 2);
        let step = (u64::from(acked).saturating_mul(u64::from(smss)) << FRACTION).checked_div(cwnd64).unwrap_or(0);
        let reno_friendly = self.prior.is_some_and(|prior| epoch.estimate >> FRACTION >= u64::from(prior));
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
    }

    /// Entering fast or SACK recovery (RFC 9438 §4.6, §4.7). `flight` excludes Limited Transmit.
    pub fn on_loss(&mut self, flight: u32) {
        self.epoch = None;
        self.w_max = Some(match self.w_max {
            Some(w_max) if self.cwnd < w_max => u32::try_from(u64::from(self.cwnd).saturating_mul(17) / 20).unwrap_or(u32::MAX),
            _ => self.cwnd,
        });
        self.prior = Some(self.cwnd);
        self.ssthresh = self.reduced(flight);
    }

    /// A retransmission timeout (RFC 5681 §3.1, RFC 9438 §4.8). `repeat` is a second timeout of
    /// the same segment, which leaves ssthresh where the first put it.
    pub fn on_timeout(&mut self, flight: u32, repeat: bool) {
        if !repeat {
            self.ssthresh = self.reduced(flight);
        }
        self.prior = Some(self.cwnd);
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

    fn avoidance(cwnd: u32, w_max: u32) -> Cc {
        let mut cc = Cc::new(1460, 0);
        cc.cwnd = cwnd;
        cc.ssthresh = cwnd;
        cc.w_max = Some(w_max);
        cc
    }

    #[test]
    fn s_cc_009_cubic_values() {
        let mut cc = avoidance(102_200, 146_000);
        cc.prior = Some(146_000);
        cc.on_ack(at(0), 0, None);
        let epoch = cc.epoch.unwrap();
        assert!(epoch.k_us.abs_diff(4_217_200) <= 1_000, "K = {} µs", epoch.k_us);
        for (t, want) in [(0u64, 102_200f64), (1_000_000, 126_554.0), (2_000_000, 139_635.0), (epoch.k_us, 146_000.0), (5_000_000, 146_280.0)] {
            let got = epoch.cubic(1460, t) as f64;
            assert!((got - want).abs() <= want * 0.005, "W({t}) = {got}, want {want}");
        }
    }

    #[test]
    fn s_cc_011_reno_friendly_region() {
        let mut cc = avoidance(14_600, 14_600);
        cc.on_ack(at(0), 0, None);
        for i in 1..=10 {
            cc.limited = true;
            cc.on_ack(at(5 * i), 1460, Some(Duration::from_millis(5)));
        }
        assert!((f64::from(cc.cwnd) - 15_355.0).abs() <= 153.0, "cwnd {}", cc.cwnd);
    }

    #[test]
    fn s_cc_013_application_limited_time_is_skipped() {
        let mut cc = avoidance(102_200, 146_000);
        cc.on_ack(at(0), 0, None);
        for ms in (100..=2_000).step_by(100) {
            cc.on_ack(at(ms), 1460, None);
        }
        let start = cc.epoch.unwrap().start;
        cc.limited = true;
        cc.on_ack(at(3_000), 1460, None);
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
            cc.on_ack(at(now), 0, Some(srtt));
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
                    cc.on_ack(at(now), n, Some(srtt));
                    let reno = cc.epoch.unwrap().estimate >> FRACTION;
                    assert!(u64::from(cc.cwnd) <= target.max(reno), "cwnd {} above target {target}", cc.cwnd);
                    acked += n;
                }
                assert!(u64::from(cc.cwnd) * 2 <= u64::from(round_start) * 3 + 2 * 1460, "{} after {round_start}", cc.cwnd);
            }
        }
    }
}
