//! ARP for Ethernet and IPv4 only (RFC 826), with the probe and announcement
//! of RFC 5227.
//!
//! Every packet is 28 bytes; bytes after the 28th are link padding and ignored
//! whatever they hold. The hardware and protocol types are checked before the
//! address lengths, so a foreign pairing is unsupported rather than malformed.
//! A request's target MAC means nothing and is exposed only so the packet
//! re-emits byte for byte; ToyOS writes zeros there.

use core::net::Ipv4Addr;

use crate::emit::{be16x2, put, BuildError};
use crate::ethernet::{FrameBody, IndividualMac, MacAddr, TxEtherType};

/// The length of an Ethernet/IPv4 ARP packet.
pub const LEN: usize = 28;

reasons! {
    /// Why an ARP packet was refused, in the order the checks run.
    ArpError {
        /// Fewer than 8 bytes, or fewer than 28 once the types are known.
        Truncated = "arp.truncated", Malformed;
        /// A hardware type other than 1 (Ethernet); IEEE 802's 6 included.
        HardwareType = "arp.hardware-type", Unsupported;
        /// A protocol type other than 0x0800.
        ProtocolType = "arp.protocol-type", Unsupported;
        /// Address lengths other than 6 and 4.
        AddressLength = "arp.address-length", Malformed;
        /// An operation other than request or reply (RARP, InARP, anything else).
        Operation = "arp.operation", Unsupported;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Request,
    Reply,
}

/// What a request or reply is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A request from 0.0.0.0, asking whether an address is in use (RFC 5227 §2.1.1).
    Probe,
    /// Sender and target IPv4 equal: a claim (RFC 5227 §2.3).
    Announcement,
    Ordinary,
}

/// An ARP packet: parsed, or to be built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Arp {
    pub operation: Operation,
    pub sender_mac: MacAddr,
    pub sender_ip: Ipv4Addr,
    pub target_mac: MacAddr,
    pub target_ip: Ipv4Addr,
}

impl Arp {
    pub fn parse(bytes: &[u8]) -> Result<Self, ArpError> {
        let (fixed, _) = bytes.split_first_chunk::<8>().ok_or(ArpError::Truncated)?;
        let [h0, h1, p0, p1, hlen, plen, o0, o1] = *fixed;
        if u16::from_be_bytes([h0, h1]) != 1 {
            return Err(ArpError::HardwareType);
        }
        if u16::from_be_bytes([p0, p1]) != 0x0800 {
            return Err(ArpError::ProtocolType);
        }
        if (hlen, plen) != (6, 4) {
            return Err(ArpError::AddressLength);
        }
        let (packet, _) = bytes.split_first_chunk::<LEN>().ok_or(ArpError::Truncated)?;
        let operation = match u16::from_be_bytes([o0, o1]) {
            1 => Operation::Request,
            2 => Operation::Reply,
            _ => return Err(ArpError::Operation),
        };
        let [_, _, _, _, _, _, _, _, a0, a1, a2, a3, a4, a5, i0, i1, i2, i3, b0, b1, b2, b3, b4, b5, j0, j1, j2, j3] =
            *packet;
        Ok(Self {
            operation,
            sender_mac: MacAddr([a0, a1, a2, a3, a4, a5]),
            sender_ip: Ipv4Addr::new(i0, i1, i2, i3),
            target_mac: MacAddr([b0, b1, b2, b3, b4, b5]),
            target_ip: Ipv4Addr::new(j0, j1, j2, j3),
        })
    }

    pub fn kind(&self) -> Kind {
        if self.operation == Operation::Request && self.sender_ip.is_unspecified() {
            Kind::Probe
        } else if self.sender_ip == self.target_ip {
            Kind::Announcement
        } else {
            Kind::Ordinary
        }
    }

    /// Who has `target`? Sent to broadcast.
    pub const fn request(sender: IndividualMac, sender_ip: Ipv4Addr, target: Ipv4Addr) -> Self {
        Self {
            operation: Operation::Request,
            sender_mac: sender.get(),
            sender_ip,
            target_mac: MacAddr::ZERO,
            target_ip: target,
        }
    }

    /// Is `candidate` in use? The sender IPv4 is 0.0.0.0 (RFC 5227 §2.1.1).
    pub const fn probe(sender: IndividualMac, candidate: Ipv4Addr) -> Self {
        Self::request(sender, Ipv4Addr::UNSPECIFIED, candidate)
    }

    /// `claimed` is ours: sender and target IPv4 both name it (RFC 5227 §2.3).
    pub const fn announcement(sender: IndividualMac, claimed: Ipv4Addr) -> Self {
        Self::request(sender, claimed, claimed)
    }

    /// The answer to `request` from the interface `sender` that holds `sender_ip`.
    pub const fn reply(sender: IndividualMac, sender_ip: Ipv4Addr, request: &Self) -> Self {
        Self {
            operation: Operation::Reply,
            sender_mac: sender.get(),
            sender_ip,
            target_mac: request.sender_mac,
            target_ip: request.sender_ip,
        }
    }

    /// Where the frame goes: a request is broadcast, a reply goes to its target.
    pub const fn frame_destination(&self) -> MacAddr {
        match self.operation {
            Operation::Request => MacAddr::BROADCAST,
            Operation::Reply => self.target_mac,
        }
    }
}

impl FrameBody for Arp {
    const ETHER_TYPE: TxEtherType = TxEtherType::Arp;

    fn length(&self) -> Result<usize, BuildError> {
        Ok(LEN)
    }

    fn write(&self, out: &mut [u8]) -> Result<(), BuildError> {
        let operation = match self.operation {
            Operation::Request => 1,
            Operation::Reply => 2,
        };
        let [o0, o1] = u16::to_be_bytes(operation);
        let out = put(out, be16x2(1, 0x0800))?;
        let out = put(out, [6, 4, o0, o1])?;
        let out = put(out, self.sender_mac.0)?;
        let out = put(out, self.sender_ip.octets())?;
        let out = put(out, self.target_mac.0)?;
        put(out, self.target_ip.octets())?;
        Ok(())
    }
}
