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
