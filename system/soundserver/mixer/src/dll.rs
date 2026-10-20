//! The delay-locked loop that tracks the device's own period grid.
//!
//! A completion record says when the device finished a period, and the mix loop
//! has to arm a timer for the next one. Sleeping a nominal period from now
//! accumulates every scheduling error there ever was; this tracks the grid the
//! device is actually on, which drifts against the host's clock by whatever the
//! two crystals differ by.

/// The estimator. Second-order, with one bandwidth for both the phase and the
/// period term.
pub struct Dll {
    /// When the next period is predicted to complete, or `None` before the
    /// first record and after a [`reset`](Self::reset).
    pub t_estimated: Option<f64>,
    /// The tracked period, in nanoseconds. Clamped to [50%, 200%] of nominal.
    pub period: f64,
    nominal_period: f64,
    bw: f64,
    /// Periods counted since the last update that no time has clocked yet.
    unclocked: u32,
}

impl Dll {
    pub fn new(nominal_period_nanos: f64) -> Self {
        Self {
            t_estimated: None,
            period: nominal_period_nanos,
            nominal_period: nominal_period_nanos,
            bw: 0.03,
            unclocked: 0,
        }
    }

    /// Forget the estimate after a pipeline re-prime; the next
    /// completion record re-initializes it.
    pub fn reset(&mut self) {
        self.t_estimated = None;
        self.period = self.nominal_period;
        self.unclocked = 0;
    }

    /// Feed one completion record: `n_periods` buffers finished, and the first
    /// `clocked` of them, in play order, end at `t_actual`. The rest completed
    /// after the message `t_actual` is the time of, so a later record's time
    /// clocks them.
    ///
    /// The timestamp belongs to the *last* grid point it clocks, counting
    /// every period since the last update, so the prediction error is
    /// measured against `t_estimated + (n-1)·period` over those `n`.
    pub fn update(&mut self, t_actual: f64, n_periods: u32, clocked: u32) {
        assert!(clocked <= n_periods, "dll: {clocked} of {n_periods} periods clocked");
        if clocked == 0 {
            self.unclocked += n_periods;
            return;
        }
        let n_periods = core::mem::replace(&mut self.unclocked, n_periods - clocked) + clocked;
        match self.t_estimated {
            None => {
                self.t_estimated = Some(t_actual + self.period);
            }
            Some(t_est) => {
                let predicted = t_est + (n_periods - 1) as f64 * self.period;
                let error = t_actual - predicted;
                let next = predicted + self.period + self.bw * error;
                // Clamp period to [50%, 200%] of nominal to prevent collapse
                self.period = (self.period + self.bw * self.bw * error)
                    .clamp(self.nominal_period * 0.5, self.nominal_period * 2.0);
                self.t_estimated = Some(next);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOMINAL: f64 = 2_902_494.0;

    /// The first record only anchors: there is no error to measure against a
    /// prediction that does not exist, so the period must still be nominal.
    #[test]
    fn the_first_record_anchors_and_decides_nothing() {
        let mut dll = Dll::new(NOMINAL);
        assert!(dll.t_estimated.is_none());
        dll.update(1_000.0, 1, 1);
        assert_eq!(dll.t_estimated, Some(1_000.0 + NOMINAL));
        assert_eq!(dll.period, NOMINAL);
    }

    /// A device running a little fast is tracked, and the estimate converges on
    /// its grid rather than on the nominal one — which is the whole reason the
    /// loop exists.
    #[test]
    fn a_grid_that_runs_fast_is_followed() {
        let real = NOMINAL * 0.999;
        let mut dll = Dll::new(NOMINAL);
        let mut t = 0.0f64;
        for _ in 0..4000 {
            t += real;
            dll.update(t, 1, 1);
        }
        assert!(
            (dll.period - real).abs() < NOMINAL * 1e-4,
            "tracked {} against a real {real}",
            dll.period
        );
        assert!(
            (dll.t_estimated.unwrap() - (t + real)).abs() < real,
            "the estimate is not on the device's grid"
        );
    }

    /// **The clamp holds in both directions.** A timestamp far off the grid is
    /// what a stalled host or a re-primed pipeline produces, and a period
    /// estimate that collapsed toward zero would arm the timer in a spin and a
    /// period that ran away would arm it past the next completion.
    #[test]
    fn a_wild_timestamp_cannot_collapse_or_run_away_the_period() {
        let mut dll = Dll::new(NOMINAL);
        dll.update(0.0, 1, 1);
        for k in 1..1000u32 {
            dll.update(k as f64 * NOMINAL * 8.0, 1, 1);
        }
        assert_eq!(dll.period, NOMINAL * 2.0);

        let mut dll = Dll::new(NOMINAL);
        dll.update(0.0, 1, 1);
        for k in 1..1000u32 {
            dll.update(k as f64 * NOMINAL * 0.01, 1, 1);
        }
        assert_eq!(dll.period, NOMINAL * 0.5);
    }

    /// A batch of `n` periods retiring on one interrupt is `n` grid points, and
    /// the timestamp belongs to the last of them. Reading it as the first
    /// scores an `(n-1)`-period error on every batched IRQ, which QEMU produces
    /// routinely.
    #[test]
    fn a_batch_is_measured_against_its_last_grid_point() {
        let mut batched = Dll::new(NOMINAL);
        batched.update(0.0, 1, 1);
        batched.update(4.0 * NOMINAL, 4, 4);

        let mut singly = Dll::new(NOMINAL);
        singly.update(0.0, 1, 1);
        for k in 1..=4 {
            singly.update(k as f64 * NOMINAL, 1, 1);
        }
        // An exact grid gives zero error either way, so the period is untouched
        // and the estimate is the same next point.
        assert_eq!(batched.period, NOMINAL);
        assert_eq!(batched.t_estimated, Some(5.0 * NOMINAL));
        assert_eq!(singly.t_estimated, Some(5.0 * NOMINAL));
    }

    /// A period a record counts past the one its time is for is clocked by the
    /// next record's time, not stamped early with this one's: the grid points
    /// the two updates measure are the ones a record per period would have.
    #[test]
    fn a_period_counted_past_its_message_is_clocked_by_the_next_record() {
        let mut carried = Dll::new(NOMINAL);
        carried.update(0.0, 1, 1);
        carried.update(1.0 * NOMINAL, 3, 1);
        assert_eq!(carried.t_estimated, Some(2.0 * NOMINAL));
        carried.update(4.0 * NOMINAL, 1, 1);
        assert_eq!(carried.t_estimated, Some(5.0 * NOMINAL));
        assert_eq!(carried.period, NOMINAL);

        // Read as clocking all three, the same records score a two-period
        // error and the period moves.
        let mut early = Dll::new(NOMINAL);
        early.update(0.0, 1, 1);
        early.update(1.0 * NOMINAL, 3, 3);
        assert_ne!(early.period, NOMINAL);
    }

    /// A record whose time clocks none of its periods updates nothing, and its
    /// periods count toward the next.
    #[test]
    fn a_record_clocking_nothing_carries_its_periods() {
        let mut dll = Dll::new(NOMINAL);
        dll.update(0.0, 1, 1);
        dll.update(9e9, 2, 0);
        assert_eq!(dll.t_estimated, Some(NOMINAL));
        dll.update(3.0 * NOMINAL, 1, 1);
        assert_eq!(dll.t_estimated, Some(4.0 * NOMINAL));
        assert_eq!(dll.period, NOMINAL);
    }

    /// A reset drops the grid entirely: the device restarts its period grid
    /// from whatever is submitted next, so an estimate carried across would
    /// read the discontinuity as drift.
    #[test]
    fn a_reset_forgets_the_grid_and_the_drift() {
        let mut dll = Dll::new(NOMINAL);
        dll.update(0.0, 1, 1);
        for k in 1..100u32 {
            dll.update(k as f64 * NOMINAL * 1.1, 1, 1);
        }
        assert_ne!(dll.period, NOMINAL);
        dll.reset();
        assert!(dll.t_estimated.is_none());
        assert_eq!(dll.period, NOMINAL);
    }
}
