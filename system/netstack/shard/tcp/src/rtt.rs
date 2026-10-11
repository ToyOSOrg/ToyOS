//! The retransmission timeout (RFC 6298 §2, §5), in integer microseconds.

use core::num::NonZeroU64;
use core::time::Duration;

pub const RTO_INITIAL: Duration = Duration::from_secs(1);
pub const RTO_MIN: Duration = Duration::from_millis(200);
pub const RTO_MAX: Duration = Duration::from_secs(60);
/// RFC 6298 (5.7): the RTO once data flows after the SYN or SYN-ACK timed out.
pub const RTO_AFTER_HANDSHAKE_LOSS: Duration = Duration::from_secs(3);
/// The clock granularity G of RFC 6298 §4: the timer's 1 ms resolution.
const GRANULARITY_US: u64 = 1_000;
/// Consecutive expiries without a valid sample after which SRTT and RTTVAR are forgotten (RFC 6298 §5).
const FORGET_AFTER: u8 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Estimate {
    srtt_us: u64,
    rttvar_us: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct Rtt {
    estimate: Option<Estimate>,
    rto: Duration,
    expiries: u8,
}

fn us(d: Duration) -> u64 {
    u64::try_from(d.as_micros()).unwrap_or(u64::MAX)
}

impl Rtt {
    pub const fn new() -> Self {
        Self { estimate: None, rto: RTO_INITIAL, expiries: 0 }
    }

    pub const fn rto(&self) -> Duration {
        self.rto
    }

    pub fn srtt(&self) -> Option<Duration> {
        self.estimate.map(|e| Duration::from_micros(e.srtt_us))
    }

    pub fn rttvar(&self) -> Option<Duration> {
        self.estimate.map(|e| Duration::from_micros(e.rttvar_us))
    }

    /// A valid sample. `weight` is RFC 7323 Appendix G's ExpectedSamples, 1 for a Karn sample.
    pub fn sample(&mut self, r: Duration, weight: u32) {
        let r = us(r);
        let n = NonZeroU64::new(u64::from(weight)).unwrap_or(NonZeroU64::MIN);
        let estimate = match self.estimate {
            None => Estimate { srtt_us: r, rttvar_us: r / 2 },
            Some(Estimate { srtt_us, rttvar_us }) => {
                let beta = n.saturating_mul(NonZeroU64::MIN.saturating_add(3));
                let alpha = n.saturating_mul(NonZeroU64::MIN.saturating_add(7));
                let deviation = srtt_us.abs_diff(r);
                Estimate {
                    rttvar_us: rttvar_us.saturating_mul(beta.get().saturating_sub(1)).saturating_add(deviation) / beta,
                    srtt_us: srtt_us.saturating_mul(alpha.get().saturating_sub(1)).saturating_add(r) / alpha,
                }
            }
        };
        self.estimate = Some(estimate);
        self.expiries = 0;
        let rto = estimate.srtt_us.saturating_add(GRANULARITY_US.max(estimate.rttvar_us.saturating_mul(4)));
        self.rto = Duration::from_micros(rto).clamp(RTO_MIN, RTO_MAX);
    }

    /// An expiry: the RTO doubles, and after three in a row the estimate is taken to be bogus.
    pub fn back_off(&mut self) {
        self.rto = self.rto.saturating_mul(2).min(RTO_MAX);
        self.expiries = self.expiries.saturating_add(1);
        if self.expiries >= FORGET_AFTER {
            self.estimate = None;
        }
    }

    pub fn set_rto(&mut self, rto: Duration) {
        self.rto = rto;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn s_rt_001_first_sample() {
        let mut rtt = Rtt::new();
        assert_eq!(rtt.rto(), ms(1000));
        rtt.sample(ms(100), 1);
        assert_eq!(rtt.estimate, Some(Estimate { srtt_us: 100_000, rttvar_us: 50_000 }));
        assert_eq!(rtt.rto(), ms(300));
    }

    #[test]
    fn s_rt_002_later_sample() {
        let mut rtt = Rtt::new();
        rtt.sample(ms(100), 1);
        rtt.sample(ms(120), 1);
        assert_eq!(rtt.estimate, Some(Estimate { srtt_us: 102_500, rttvar_us: 42_500 }));
        assert_eq!(rtt.rto(), Duration::from_micros(272_500));
    }

    #[test]
    fn s_rt_003_the_200_ms_floor() {
        let mut rtt = Rtt::new();
        rtt.sample(ms(1), 1);
        assert_eq!(rtt.estimate, Some(Estimate { srtt_us: 1_000, rttvar_us: 500 }));
        assert_eq!(rtt.rto(), ms(200));
    }

    #[test]
    fn s_rt_004_the_60_s_ceiling() {
        let mut rtt = Rtt::new();
        rtt.set_rto(ms(40_000));
        rtt.back_off();
        assert_eq!(rtt.rto(), ms(60_000));
        rtt.back_off();
        assert_eq!(rtt.rto(), ms(60_000));
    }

    #[test]
    fn s_rt_012_appendix_g_weights() {
        let mut rtt = Rtt { estimate: Some(Estimate { srtt_us: 100_000, rttvar_us: 50_000 }), rto: ms(300), expiries: 0 };
        rtt.sample(ms(200), 5);
        assert_eq!(rtt.estimate, Some(Estimate { srtt_us: 102_500, rttvar_us: 52_500 }));
    }
}
