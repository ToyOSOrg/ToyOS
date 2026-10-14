//! The MAC header (§9.2.3, §9.2.4) of the frames a station handles: every management subtype it
//! takes part in, BlockAckReq and BlockAck, and Data, Null, QoS Data and QoS Null.
//!
//! The input is one MPDU without its FCS. A body is handed back undecoded, because a protected
//! one is ciphertext until the caller removes its protection: management bodies decode in
//! [`crate::mgmt`], data bodies in [`crate::data`].

use crate::control::{BlockAck, BlockAckReq};
use crate::read::Reader;
use crate::{Address, Refused, Seq};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Frame<'a> {
    Management(Management<'a>),
    BlockAckReq(BlockAckReq),
    BlockAck(BlockAck),
    Data(Data<'a>),
}

/// The Frame Control flags a receiver acts on (§9.2.4.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Flags {
    pub retry: bool,
    pub power_management: bool,
    pub more_data: bool,
    pub protected: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManagementSubtype {
    AssociationRequest,
    AssociationResponse,
    ReassociationRequest,
    ReassociationResponse,
    ProbeRequest,
    ProbeResponse,
    Beacon,
    Disassociation,
    Authentication,
    Deauthentication,
    Action,
    ActionNoAck,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Management<'a> {
    pub flags: Flags,
    pub subtype: ManagementSubtype,
    pub duration: u16,
    pub da: Address,
    pub sa: Address,
    pub bssid: Address,
    pub seq: Seq,
    pub ht_control: Option<u32>,
    pub body: &'a [u8],
}

/// To DS and From DS (Table 9-4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ds {
    /// Station to station within one BSS.
    Neither,
    /// Station to access point.
    ToDs,
    /// Access point to station.
    FromDs,
    /// The four-address format, with its Address 4.
    Both(Address),
}

/// The QoS Control field (§9.2.4.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QosControl(pub u16);

impl QosControl {
    pub const fn tid(self) -> u8 {
        let [low, _] = self.0.to_le_bytes();
        low & 0x0F
    }

    /// The Ack Policy subfield, bits 5 and 6 (Table 9-11).
    pub const fn ack_policy(self) -> u8 {
        let [low, _] = self.0.to_le_bytes();
        (low >> 5) & 0b11
    }

    /// The A-MSDU Present subfield, bit 7.
    pub const fn amsdu(self) -> bool {
        self.0 & 0x0080 != 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Data<'a> {
    pub flags: Flags,
    pub ds: Ds,
    pub duration: u16,
    pub addr1: Address,
    pub addr2: Address,
    pub addr3: Address,
    pub seq: Seq,
    pub qos: Option<QosControl>,
    pub ht_control: Option<u32>,
    /// A Null or QoS Null frame, which carries no MSDU.
    pub null: bool,
    pub body: &'a [u8],
}

impl Data<'_> {
    /// The receiver, which Address 1 always is.
    pub const fn ra(&self) -> Address {
        self.addr1
    }

    /// The transmitter, which Address 2 always is.
    pub const fn ta(&self) -> Address {
        self.addr2
    }

    /// The MSDU's destination (Table 9-30).
    pub const fn da(&self) -> Address {
        match self.ds {
            Ds::Neither | Ds::FromDs => self.addr1,
            Ds::ToDs | Ds::Both(_) => self.addr3,
        }
    }

    /// The MSDU's source (Table 9-30).
    pub const fn sa(&self) -> Address {
        match self.ds {
            Ds::Neither | Ds::ToDs => self.addr2,
            Ds::FromDs => self.addr3,
            Ds::Both(addr4) => addr4,
        }
    }
}

impl<'a> Frame<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Refused> {
        let mut r = Reader::new(bytes);
        let [fc0, fc1] = r.array()?;
        if fc0 & 0b11 != 0 {
            return Err(Refused::ProtocolVersion);
        }
        let subtype = fc0 >> 4;
        let flags = Flags {
            retry: fc1 & 0x08 != 0,
            power_management: fc1 & 0x10 != 0,
            more_data: fc1 & 0x20 != 0,
            protected: fc1 & 0x40 != 0,
        };
        let ds_bits = fc1 & 0b11;
        let more_fragments = fc1 & 0x04 != 0;
        let order = fc1 & 0x80 != 0;
        let duration = r.u16()?;
        match (fc0 >> 2) & 0b11 {
            0 => {
                let subtype = management_subtype(subtype)?;
                if ds_bits != 0 {
                    return Err(Refused::ManagementWithDs);
                }
                let da = r.address()?;
                let sa = r.address()?;
                let bssid = r.address()?;
                let seq = sequence_control(r.u16()?, more_fragments)?;
                if sa.is_group() {
                    return Err(Refused::GroupTransmitter);
                }
                let ht_control = if order { Some(r.u32()?) } else { None };
                Ok(Self::Management(Management {
                    flags,
                    subtype,
                    duration,
                    da,
                    sa,
                    bssid,
                    seq,
                    ht_control,
                    body: r.rest(),
                }))
            }
            1 => match subtype {
                8 => BlockAckReq::parse(r.rest()).map(Self::BlockAckReq),
                9 => BlockAck::parse(r.rest()).map(Self::BlockAck),
                _ => Err(Refused::UnhandledControl),
            },
            2 => {
                let (qos, null) = match subtype {
                    0 => (false, false),
                    4 => (false, true),
                    8 => (true, false),
                    12 => (true, true),
                    13 => return Err(Refused::ReservedSubtype),
                    _ => return Err(Refused::ContentionFreeData),
                };
                if order && !qos {
                    return Err(Refused::StrictlyOrdered);
                }
                let addr1 = r.address()?;
                let addr2 = r.address()?;
                let addr3 = r.address()?;
                let seq = sequence_control(r.u16()?, more_fragments)?;
                let ds = match ds_bits {
                    0 => Ds::Neither,
                    1 => Ds::ToDs,
                    2 => Ds::FromDs,
                    _ => Ds::Both(r.address()?),
                };
                if addr2.is_group() {
                    return Err(Refused::GroupTransmitter);
                }
                let qos = if qos { Some(QosControl(r.u16()?)) } else { None };
                let ht_control = if order { Some(r.u32()?) } else { None };
                Ok(Self::Data(Data {
                    flags,
                    ds,
                    duration,
                    addr1,
                    addr2,
                    addr3,
                    seq,
                    qos,
                    ht_control,
                    null,
                    body: r.rest(),
                }))
            }
            _ => Err(Refused::ExtensionFrame),
        }
    }
}

/// Table 9-1. Timing Advertisement and ATIM are no station's business in an infrastructure BSS.
fn management_subtype(subtype: u8) -> Result<ManagementSubtype, Refused> {
    use ManagementSubtype::*;
    Ok(match subtype {
        0 => AssociationRequest,
        1 => AssociationResponse,
        2 => ReassociationRequest,
        3 => ReassociationResponse,
        4 => ProbeRequest,
        5 => ProbeResponse,
        8 => Beacon,
        10 => Disassociation,
        11 => Authentication,
        12 => Deauthentication,
        13 => Action,
        14 => ActionNoAck,
        7 | 15 => return Err(Refused::ReservedSubtype),
        _ => return Err(Refused::UnhandledManagement),
    })
}

/// A fragment is refused rather than reassembled: no reassembly buffer exists to mix fragments
/// of different keys or frames in (Vanhoef, "Fragment and Forge", USENIX Security 2021, §5).
fn sequence_control(field: u16, more_fragments: bool) -> Result<Seq, Refused> {
    if more_fragments || field & 0x000F != 0 {
        return Err(Refused::Fragmented);
    }
    Ok(Seq::new(field >> 4))
}
