//! IPv4 address classes and prefixes (RFC 1122 §3.2.1.3, RFC 6890, RFC 4632, RFC 3021).

use core::net::Ipv4Addr;

/// Not 0.0.0.0/8, 127.0.0.0/8, 224.0.0.0/4 or 240.0.0.0/4 (which holds 255.255.255.255): an
/// address that can name one host.
pub const fn is_host(addr: Ipv4Addr) -> bool {
    let [first, ..] = addr.octets();
    !(first == 0 || first == 127 || first >= 224)
}

/// Never a destination: 0.0.0.0/8, 127.0.0.0/8, and 240.0.0.0/4 but the limited broadcast.
pub const fn is_martian(addr: Ipv4Addr) -> bool {
    let [first, ..] = addr.octets();
    first == 0 || first == 127 || (first >= 240 && !addr.is_broadcast())
}

/// An address and the length of its prefix, 1 to 32.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cidr {
    addr: Ipv4Addr,
    len: u8,
}

impl Cidr {
    pub const fn new(addr: Ipv4Addr, len: u8) -> Option<Self> {
        if matches!(len, 1..=32) {
            Some(Self { addr, len })
        } else {
            None
        }
    }

    /// The prefix a subnet mask names: contiguous ones, then zeros (RFC 4632 §3.1).
    pub fn from_mask(addr: Ipv4Addr, mask: Ipv4Addr) -> Option<Self> {
        let bits = u32::from(mask);
        let len = bits.leading_ones();
        if len.checked_add(bits.trailing_zeros()) != Some(32) {
            return None;
        }
        Self::new(addr, u8::try_from(len).ok()?)
    }

    pub const fn addr(self) -> Ipv4Addr {
        self.addr
    }

    pub const fn prefix_len(self) -> u8 {
        self.len
    }

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
