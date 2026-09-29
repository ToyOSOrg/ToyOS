//! An interface's address records, and the address classes every rule reads (RFC 1122
//! §3.2.1.3, RFC 6890, RFC 3021).

use core::net::Ipv4Addr;

use toyos_net_wire::Instant;

/// Not 0.0.0.0/8, 127.0.0.0/8, 224.0.0.0/4 or 240.0.0.0/4 (which holds 255.255.255.255): an
/// address that can name one host.
pub(crate) const fn is_host(addr: Ipv4Addr) -> bool {
    let [first, ..] = addr.octets();
    !(first == 0 || first == 127 || first >= 224)
}

/// 240.0.0.0/4 but the limited broadcast.
pub(crate) const fn is_class_e(addr: Ipv4Addr) -> bool {
    let [first, ..] = addr.octets();
    first >= 240 && !addr.is_broadcast()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Cidr {
    pub addr: Ipv4Addr,
    /// 1 to 32.
    pub len: u8,
}

impl Cidr {
    fn mask(self) -> u32 {
        u32::MAX.checked_shl(u32::from(32u8.saturating_sub(self.len))).unwrap_or(0)
    }

    pub fn contains(self, other: Ipv4Addr) -> bool {
        (u32::from(self.addr) ^ u32::from(other)) & self.mask() == 0
    }

    pub fn network(self) -> Ipv4Addr {
        Ipv4Addr::from(u32::from(self.addr) & self.mask())
    }

    /// Host bits all one; a /31 or /32 has none (RFC 3021 §2.2.1).
    pub fn broadcast(self) -> Option<Ipv4Addr> {
        (self.len <= 30).then(|| Ipv4Addr::from(u32::from(self.addr) | !self.mask()))
    }

    /// The network or directed-broadcast address of a prefix of /30 or shorter, which no host
    /// may use.
    pub fn is_edge(self, addr: Ipv4Addr) -> bool {
        self.len <= 30 && self.contains(addr) && (addr == self.network() || Some(addr) == self.broadcast())
    }
}

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
    /// Usable, with the announcements still owed and whether one waits to leave.
    Usable { assigned: bool, owed: u8, queued: bool },
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
