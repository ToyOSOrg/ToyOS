//! ARP for Ethernet and IPv4 (RFC 826), with RFC 5227's probe and announcement.

use core::net::Ipv4Addr;

use crate::emit::{be16x2, put, BuildError};
use crate::ethernet::{FrameBody, IndividualMac, MacAddr, TxEtherType};

pub const LEN: usize = 28;

reasons! {
    ArpError {
        Truncated = "arp.truncated", Malformed;
        HardwareType = "arp.hardware-type", Unsupported;
        ProtocolType = "arp.protocol-type", Unsupported;
        AddressLength = "arp.address-length", Malformed;
        Operation = "arp.operation", Unsupported;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Request,
    Reply,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Probe,
    Announcement,
    Ordinary,
}

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
        // After the types, so a foreign pairing is unsupported rather than malformed.
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

    pub const fn request(sender: IndividualMac, sender_ip: Ipv4Addr, target: Ipv4Addr) -> Self {
        Self {
            operation: Operation::Request,
            sender_mac: sender.get(),
            sender_ip,
            target_mac: MacAddr::ZERO,
            target_ip: target,
        }
    }

    pub const fn probe(sender: IndividualMac, candidate: Ipv4Addr) -> Self {
        Self::request(sender, Ipv4Addr::UNSPECIFIED, candidate)
    }

    pub const fn announcement(sender: IndividualMac, claimed: Ipv4Addr) -> Self {
        Self::request(sender, claimed, claimed)
    }

    pub const fn reply(sender: IndividualMac, sender_ip: Ipv4Addr, request: &Self) -> Self {
        Self {
            operation: Operation::Reply,
            sender_mac: sender.get(),
            sender_ip,
            target_mac: request.sender_mac,
            target_ip: request.sender_ip,
        }
    }

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
