//! Elements (9.4.2): the TLV walk of a body's element list, and the elements a station decides
//! by, each checked against its definition and allowed once.
//!
//! An element this module does not type is skipped, as 10.28.9 has a receiver ignore what it
//! does not know; one it types that breaks its definition refuses the whole list.

mod country;
mod ht;
mod rsn;
mod tim;

pub use country::{Country, Triplet, Triplets};
pub use ht::{HtCapabilities, HtOperation, SecondaryChannel, VhtCapabilities, VhtChannelWidth, VhtOperation};
pub use rsn::{Rsn, Rsnx, Suite, Suites};
pub use tim::Tim;

use crate::read::Reader;
use crate::Refused;

/// An element's identifier: an Element ID, or 255 and its Element ID Extension (9.4.2.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Id {
    Base(u8),
    Extension(u8),
}

impl Id {
    pub const SSID: Self = Self::Base(0);
    pub const SUPPORTED_RATES: Self = Self::Base(1);
    pub const DSSS_PARAMETER_SET: Self = Self::Base(3);
    pub const TIM: Self = Self::Base(5);
    pub const COUNTRY: Self = Self::Base(7);
    pub const HT_CAPABILITIES: Self = Self::Base(45);
    pub const RSN: Self = Self::Base(48);
    pub const EXTENDED_SUPPORTED_RATES: Self = Self::Base(50);
    pub const HT_OPERATION: Self = Self::Base(61);
    pub const EXTENDED_CAPABILITIES: Self = Self::Base(127);
    pub const VHT_CAPABILITIES: Self = Self::Base(191);
    pub const VHT_OPERATION: Self = Self::Base(192);
    pub const RSNX: Self = Self::Base(244);
    const EXTENSION: u8 = 255;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Element<'a> {
    pub id: Id,
    /// The information, after an extension's Element ID Extension octet.
    pub body: &'a [u8],
}

/// The elements of a list in order; the first malformed one ends it.
pub struct Iter<'a> {
    r: Reader<'a>,
}

pub const fn iter(elements: &[u8]) -> Iter<'_> {
    Iter { r: Reader::new(elements) }
}

impl<'a> Iterator for Iter<'a> {
    type Item = Result<Element<'a>, Refused>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.r.is_empty() {
            return None;
        }
        let next = self.read();
        if next.is_err() {
            self.r = Reader::new(&[]);
        }
        Some(next)
    }
}

impl<'a> Iter<'a> {
    fn read(&mut self) -> Result<Element<'a>, Refused> {
        let [id, len] = self.r.array().map_err(|_| Refused::ElementPastBody)?;
        let body = self.r.take(usize::from(len)).map_err(|_| Refused::ElementPastBody)?;
        if id != Id::EXTENSION {
            return Ok(Element { id: Id::Base(id), body });
        }
        let (&ext, body) = body.split_first().ok_or(Refused::ExtensionWithoutId)?;
        Ok(Element { id: Id::Extension(ext), body })
    }
}

/// A rate in units of 500 kb/s, or a BSS membership selector (9.4.2.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rate {
    /// In the BSSBasicRateSet, or for a selector, required of a member.
    pub basic: bool,
    pub value: u8,
}

impl Rate {
    /// BSS membership selectors (Table 9-78).
    pub const HT_PHY: u8 = 127;
    pub const VHT_PHY: u8 = 126;
    pub const SAE_H2E_ONLY: u8 = 123;
}

/// The Supported Rates and BSS Membership Selectors element and its Extended continuation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rates<'a> {
    supported: &'a [u8],
    extended: &'a [u8],
}

impl Rates<'_> {
    pub fn iter(&self) -> impl Iterator<Item = Rate> + '_ {
        self.supported.iter().chain(self.extended).map(|&b| Rate { basic: b & 0x80 != 0, value: b & 0x7F })
    }
}

/// The elements of one list a station decides by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Elements<'a> {
    pub ssid: Option<&'a [u8]>,
    pub rates: Option<Rates<'a>>,
    pub dsss_channel: Option<u8>,
    pub tim: Option<Tim<'a>>,
    pub country: Option<Country<'a>>,
    pub rsn: Option<Rsn<'a>>,
    pub rsnx: Option<Rsnx<'a>>,
    pub ht_capabilities: Option<HtCapabilities>,
    pub ht_operation: Option<HtOperation>,
    pub vht_capabilities: Option<VhtCapabilities>,
    pub vht_operation: Option<VhtOperation>,
    pub extended_capabilities: Option<ExtendedCapabilities<'a>>,
}

/// The Extended Capabilities element: bit n of its field, absent past its length, is 0 (9.4.2.26).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtendedCapabilities<'a>(pub &'a [u8]);

impl ExtendedCapabilities<'_> {
    pub fn bit(&self, n: usize) -> bool {
        self.0.get(n / 8).is_some_and(|octet| octet >> (n % 8) & 1 != 0)
    }
}

fn once<T>(slot: &mut Option<T>, value: T) -> Result<(), Refused> {
    if slot.is_some() {
        return Err(Refused::DuplicateElement);
    }
    *slot = Some(value);
    Ok(())
}

impl<'a> Elements<'a> {
    pub fn parse(list: &'a [u8]) -> Result<Self, Refused> {
        let mut e = Self::default();
        let mut supported: Option<&'a [u8]> = None;
        let mut extended: Option<&'a [u8]> = None;
        for element in iter(list) {
            let Element { id, body } = element?;
            match id {
                Id::SSID if body.len() > 32 => return Err(Refused::SsidLength),
                Id::SSID => once(&mut e.ssid, body)?,
                Id::SUPPORTED_RATES if body.is_empty() || body.len() > 8 => return Err(Refused::RateCount),
                Id::SUPPORTED_RATES => once(&mut supported, body)?,
                Id::EXTENDED_SUPPORTED_RATES if body.is_empty() => return Err(Refused::ElementLength),
                Id::EXTENDED_SUPPORTED_RATES => once(&mut extended, body)?,
                Id::DSSS_PARAMETER_SET => match body {
                    &[channel] => once(&mut e.dsss_channel, channel)?,
                    _ => return Err(Refused::ElementLength),
                },
                Id::TIM => once(&mut e.tim, Tim::parse(body)?)?,
                Id::COUNTRY => once(&mut e.country, Country::parse(body)?)?,
                Id::RSN => once(&mut e.rsn, Rsn::parse(body)?)?,
                Id::RSNX => once(&mut e.rsnx, Rsnx::parse(body)?)?,
                Id::HT_CAPABILITIES => once(&mut e.ht_capabilities, HtCapabilities::parse(body)?)?,
                Id::HT_OPERATION => once(&mut e.ht_operation, HtOperation::parse(body)?)?,
                Id::VHT_CAPABILITIES => once(&mut e.vht_capabilities, VhtCapabilities::parse(body)?)?,
                Id::VHT_OPERATION => once(&mut e.vht_operation, VhtOperation::parse(body)?)?,
                Id::EXTENDED_CAPABILITIES => once(&mut e.extended_capabilities, ExtendedCapabilities(body))?,
                Id::Base(_) | Id::Extension(_) => {}
            }
        }
        e.rates = match (supported, extended) {
            (None, None) => None,
            (supported, extended) => Some(Rates { supported: supported.unwrap_or(&[]), extended: extended.unwrap_or(&[]) }),
        };
        Ok(e)
    }
}
