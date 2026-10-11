//! One deadline per state object, fired in deadline order, ties by kind — neighbour, ACD, IGMP —
//! and then by interface and address.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;
use core::net::Ipv4Addr;

use toyos_net_wire::Instant;

use crate::{igmp, IfIndex};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Timer {
    Neighbour(IfIndex, Ipv4Addr),
    Acd(IfIndex, Ipv4Addr),
    Igmp(IfIndex, igmp::Timer),
}

#[derive(Debug, Default)]
pub(crate) struct Timers {
    at: BTreeMap<Timer, Instant>,
    order: BTreeSet<(Instant, Timer)>,
}

impl Timers {
    pub fn arm(&mut self, timer: Timer, at: Instant) {
        self.cancel(timer);
        self.at.insert(timer, at);
        self.order.insert((at, timer));
    }

    pub fn cancel(&mut self, timer: Timer) {
        if let Some(at) = self.at.remove(&timer) {
            self.order.remove(&(at, timer));
        }
    }

    pub fn get(&self, timer: Timer) -> Option<Instant> {
        self.at.get(&timer).copied()
    }

    pub fn next(&self) -> Option<Instant> {
        self.order.first().map(|&(at, _)| at)
    }

    /// Every timer due at `now`, in order and disarmed, so what one re-arms fires in a later call:
    /// a jumped clock fires each state object once.
    pub fn due(&mut self, now: Instant) -> Vec<Timer> {
        let mut due = Vec::new();
        while let Some(&(at, timer)) = self.order.first() {
            if at > now {
                break;
            }
            self.order.pop_first();
            self.at.remove(&timer);
            due.push(timer);
        }
        due
    }

    /// Cancels every timer of `iface`.
    pub fn cancel_iface(&mut self, iface: IfIndex, kind: impl Fn(&Timer) -> bool) {
        let doomed: Vec<Timer> = self
            .at
            .keys()
            .filter(|t| match t {
                Timer::Neighbour(i, _) | Timer::Acd(i, _) | Timer::Igmp(i, _) => *i == iface,
            })
            .filter(|t| kind(t))
            .copied()
            .collect();
        for timer in doomed {
            self.cancel(timer);
        }
    }
}
