//! The Country element (§9.4.2.8): a country string, then triplets, then a pad octet of 0 where
//! one makes the length even.

use crate::Refused;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Country<'a> {
    /// Two upper-case ISO 3166-1 letters, then the environment or table octet.
    pub string: [u8; 3],
    triplets: &'a [u8],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Triplet {
    /// Channels `first_channel` onward, `channels` of them, at no more than `max_power_dbm`.
    Subband { first_channel: u8, channels: u8, max_power_dbm: i8 },
    /// The operating class the subband triplets after it are in.
    Operating { extension_id: u8, class: u8, coverage_class: u8 },
}

/// A first octet of at least this opens an Operating triplet.
const OPERATING_EXTENSION: u8 = 201;
const LAST_CHANNEL: u16 = 200;

impl<'a> Country<'a> {
    pub(super) fn parse(body: &'a [u8]) -> Result<Self, Refused> {
        let Some((&[a, b, environment], rest)) = body.split_first_chunk::<3>() else {
            return Err(Refused::ElementLength);
        };
        if !a.is_ascii_uppercase() || !b.is_ascii_uppercase() {
            return Err(Refused::CountryString);
        }
        let triplets = match rest.len() % 3 {
            0 => rest,
            1 => match rest.split_last() {
                Some((0, triplets)) => triplets,
                _ => return Err(Refused::CountryPad),
            },
            _ => return Err(Refused::CountryTriplet),
        };
        if triplets.is_empty() {
            return Err(Refused::ElementLength);
        }
        for &[first, channels, _] in triplets.as_chunks::<3>().0 {
            let past_last = u16::from(first).saturating_add(u16::from(channels)) > LAST_CHANNEL + 1;
            if first < OPERATING_EXTENSION && (first == 0 || channels == 0 || past_last) {
                return Err(Refused::CountryChannels);
            }
        }
        Ok(Self { string: [a, b, environment], triplets })
    }

    pub fn triplets(&self) -> Triplets<'a> {
        Triplets(self.triplets.as_chunks::<3>().0.iter())
    }
}

pub struct Triplets<'a>(core::slice::Iter<'a, [u8; 3]>);

impl Iterator for Triplets<'_> {
    type Item = Triplet;

    fn next(&mut self) -> Option<Triplet> {
        let &[a, b, c] = self.0.next()?;
        Some(if a >= OPERATING_EXTENSION {
            Triplet::Operating { extension_id: a, class: b, coverage_class: c }
        } else {
            Triplet::Subband { first_channel: a, channels: b, max_power_dbm: i8::from_le_bytes([c]) }
        })
    }
}
