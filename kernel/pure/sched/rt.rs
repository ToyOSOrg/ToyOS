//! The realtime class's two limits: each CPU's periodically replenished
//! allowance, which protects ordinary work on that CPU from aggregate realtime
//! demand, and each job owner's runaway record, which identifies one client's
//! runaway workers. Neither is a reservation.

use crate::sched::hw::Nanos;

/// One CPU's realtime allowance: at most `budget` ns of realtime execution in
/// each `interval`, charged where the execution happened.
///
/// Unused allowance is not banked and nothing but the interval boundary refills
/// it, so yield, wake, registration and migration cannot reset it. Execution
/// past exhaustion (a late timer, a non-preemptible section) is debt carried
/// into the next interval. Refill is lazy: an interval boundary nobody observes
/// costs nothing.
#[derive(Clone, Copy, Debug)]
pub struct Allowance {
    budget: u64,
    interval: u64,
    /// End of the current interval.
    boundary: Nanos,
    remaining: i64,
}

impl Allowance {
    pub fn new(budget: u64, interval: u64, now: Nanos) -> Self {
        assert!(budget > 0 && budget <= interval, "rt: budget {budget} outside (0, {interval}]");
        Self { budget, interval, boundary: now.after(interval), remaining: budget as i64 }
    }

    /// Bring the allowance to the interval containing `now`.
    pub fn observe(&mut self, now: Nanos) {
        if now < self.boundary {
            return;
        }
        let passed = now.since(self.boundary) / self.interval + 1;
        self.boundary = self.boundary.after(passed * self.interval);
        let refill = (passed as i64).saturating_mul(self.budget as i64);
        self.remaining = self.remaining.min(0).saturating_add(refill).min(self.budget as i64);
    }

    /// Realtime execution over `[from, to)`, split at every boundary it crosses.
    pub fn charge(&mut self, from: Nanos, to: Nanos) {
        let mut t = from;
        while t < to {
            self.observe(t);
            let end = to.min(self.boundary);
            self.remaining -= end.since(t) as i64;
            t = end;
        }
        self.observe(to);
    }

    pub fn exhausted(&self) -> bool {
        self.remaining <= 0
    }

    /// Realtime execution this interval still admits.
    pub fn remaining(&self) -> u64 {
        self.remaining.max(0) as u64
    }

    /// When the current interval ends.
    pub fn boundary(&self) -> Nanos {
        self.boundary
    }
}

/// A job owner's runaway record, kept for the owner's whole life: closing and
/// reopening a stream or granting a new worker does not erase it.
///
/// A worker that executes `runaway_ns` of realtime time without blocking is a
/// runaway — a callback blocks every period, so an underrun or aggregate
/// overload never reaches this. `strikes_to_revoke` runaways end the owner's
/// realtime eligibility.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RtOwner {
    pub strikes: u32,
    pub revoked: bool,
}

impl RtOwner {
    /// Record one runaway; `true` when it revokes the owner.
    pub fn strike(&mut self, strikes_to_revoke: u32) -> bool {
        self.strikes += 1;
        if self.strikes >= strikes_to_revoke && !self.revoked {
            self.revoked = true;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000_000;

    #[test]
    fn unused_allowance_is_not_banked() {
        let mut a = Allowance::new(3 * MS, 5 * MS, Nanos(0));
        a.observe(Nanos(50 * MS));
        assert_eq!(a.remaining(), 3 * MS);
        assert_eq!(a.boundary(), Nanos(55 * MS));
    }

    #[test]
    fn execution_is_charged_to_the_interval_it_happened_in() {
        let mut a = Allowance::new(3 * MS, 5 * MS, Nanos(0));
        // 1 ms before the boundary, 2 ms after it.
        a.charge(Nanos(4 * MS), Nanos(7 * MS));
        assert_eq!(a.remaining(), MS);
        assert_eq!(a.boundary(), Nanos(10 * MS));
    }

    #[test]
    fn an_overrun_is_debt_in_the_next_interval() {
        let mut a = Allowance::new(3 * MS, 5 * MS, Nanos(0));
        a.charge(Nanos(0), Nanos(4 * MS));
        assert!(a.exhausted());
        a.observe(Nanos(5 * MS));
        assert_eq!(a.remaining(), 2 * MS);
    }

    #[test]
    fn strikes_survive_and_revoke_once() {
        let mut o = RtOwner::default();
        assert!(!o.strike(3));
        assert!(!o.strike(3));
        assert!(o.strike(3));
        assert!(!o.strike(3));
        assert!(o.revoked);
    }
}
