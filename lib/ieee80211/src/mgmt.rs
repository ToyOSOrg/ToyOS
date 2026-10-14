//! Management frame bodies (§9.3.3): the fixed fields of each subtype, and its elements still as
//! bytes for [`crate::element::Elements::parse`].
//!
//! An Authentication frame's fields past its status, an SAE exchange's among them, and an Action
//! frame's past its category are its algorithm's or category's to read.

use crate::frame::ManagementSubtype;
use crate::read::Reader;
use crate::{Address, Refused};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Body<'a> {
    Beacon(Beacon<'a>),
    ProbeResponse(Beacon<'a>),
    ProbeRequest { elements: &'a [u8] },
    Authentication(Authentication<'a>),
    Deauthentication { reason: u16, rest: &'a [u8] },
    Disassociation { reason: u16, rest: &'a [u8] },
    AssociationRequest(AssociationRequest<'a>),
    ReassociationRequest(AssociationRequest<'a>),
    AssociationResponse(AssociationResponse<'a>),
    ReassociationResponse(AssociationResponse<'a>),
    Action { category: u8, rest: &'a [u8] },
    ActionNoAck { category: u8, rest: &'a [u8] },
}

/// A Beacon or Probe Response (§9.3.3.2, §9.3.3.10).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Beacon<'a> {
    pub timestamp: u64,
    /// In time units of 1,024 µs, never 0.
    pub interval: u16,
    pub capability: u16,
    pub elements: &'a [u8],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Authentication<'a> {
    pub algorithm: u16,
    pub transaction: u16,
    pub status: u16,
    pub rest: &'a [u8],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AssociationRequest<'a> {
    pub capability: u16,
    pub listen_interval: u16,
    /// A reassociation's Current AP Address.
    pub current_ap: Option<Address>,
    pub elements: &'a [u8],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AssociationResponse<'a> {
    pub capability: u16,
    pub status: u16,
    /// 1 to 2007 when the status is success, and absent otherwise.
    pub aid: Option<u16>,
    pub elements: &'a [u8],
}

/// The Authentication Algorithm Number of SAE (§9.4.1.1).
pub const ALGORITHM_SAE: u16 = 3;
pub const ALGORITHM_OPEN: u16 = 0;
pub const STATUS_SUCCESS: u16 = 0;

impl<'a> Body<'a> {
    pub fn parse(subtype: ManagementSubtype, body: &'a [u8]) -> Result<Self, Refused> {
        use ManagementSubtype as S;
        let mut r = Reader::new(body);
        Ok(match subtype {
            S::Beacon => Self::Beacon(Beacon::read(&mut r)?),
            S::ProbeResponse => Self::ProbeResponse(Beacon::read(&mut r)?),
            S::ProbeRequest => Self::ProbeRequest { elements: body },
            S::Authentication => Self::Authentication(Authentication {
                algorithm: r.u16()?,
                transaction: r.u16()?,
                status: r.u16()?,
                rest: r.rest(),
            }),
            S::Deauthentication => Self::Deauthentication { reason: r.u16()?, rest: r.rest() },
            S::Disassociation => Self::Disassociation { reason: r.u16()?, rest: r.rest() },
            S::AssociationRequest => Self::AssociationRequest(AssociationRequest::read(&mut r, false)?),
            S::ReassociationRequest => Self::ReassociationRequest(AssociationRequest::read(&mut r, true)?),
            S::AssociationResponse => Self::AssociationResponse(AssociationResponse::read(&mut r)?),
            S::ReassociationResponse => Self::ReassociationResponse(AssociationResponse::read(&mut r)?),
            S::Action => Self::Action { category: r.u8()?, rest: r.rest() },
            S::ActionNoAck => Self::ActionNoAck { category: r.u8()?, rest: r.rest() },
        })
    }
}

impl<'a> Beacon<'a> {
    fn read(r: &mut Reader<'a>) -> Result<Self, Refused> {
        let timestamp = r.u64()?;
        let interval = r.u16()?;
        let capability = r.u16()?;
        if interval == 0 {
            return Err(Refused::ZeroBeaconInterval);
        }
        Ok(Self { timestamp, interval, capability, elements: r.rest() })
    }
}

impl<'a> AssociationRequest<'a> {
    fn read(r: &mut Reader<'a>, reassociation: bool) -> Result<Self, Refused> {
        let capability = r.u16()?;
        let listen_interval = r.u16()?;
        let current_ap = if reassociation { Some(r.address()?) } else { None };
        Ok(Self { capability, listen_interval, current_ap, elements: r.rest() })
    }
}

impl<'a> AssociationResponse<'a> {
    fn read(r: &mut Reader<'a>) -> Result<Self, Refused> {
        let capability = r.u16()?;
        let status = r.u16()?;
        // The two most significant bits of the AID field are set (§9.4.1.8).
        let aid = r.u16()? & 0x3FFF;
        let aid = match status {
            STATUS_SUCCESS if (1..=2007).contains(&aid) => Some(aid),
            STATUS_SUCCESS => return Err(Refused::AssociationId),
            _ => None,
        };
        Ok(Self { capability, status, aid, elements: r.rest() })
    }
}
