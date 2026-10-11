//! An interface's address records.

use toyos_net_wire::addr::Cidr;
use toyos_net_wire::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddrState {
    /// Probed for conflicts: not a source, not for us, answers no ARP.
    Tentative,
    /// Usable from its first announcement (RFC 5227 §2.3).
    Announcing,
    Assigned,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    /// Probing: probes gone so far, and whether one waits to leave.
    Tentative { probes: u8, queued: bool },
    /// Usable, with the announcements still owed, whether one waits to leave, and whether a
    /// defence waits to leave.
    Usable { assigned: bool, owed: u8, queued: bool, defending: bool },
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Address {
    pub cidr: Cidr,
    pub phase: Phase,
    /// The last conflicting packet this address was defended against (RFC 5227 §2.4 (b)).
    pub defended: Option<Instant>,
}

impl Address {
    pub fn state(&self) -> AddrState {
        match self.phase {
            Phase::Tentative { .. } => AddrState::Tentative,
            Phase::Usable { assigned: false, .. } => AddrState::Announcing,
            Phase::Usable { assigned: true, .. } => AddrState::Assigned,
        }
    }

    pub fn usable(&self) -> bool {
        matches!(self.phase, Phase::Usable { .. })
    }
}
