//! The RSN element (§9.4.2.23) and the RSN Extension element (§9.4.2.240).
//!
//! An RSNE's fields after its version are each optional, but only from the end: one present
//! needs every one before it. Table 9-130 does not mark the RSNE extensible, so an octet past its
//! last field is refused. The RSNXE is extensible: octets past its field are ignored, as §10.28.8
//! requires.

use crate::read::Reader;
use crate::Refused;

/// A cipher or AKM suite selector: an OUI or CID and a type (§9.4.2.23.2, §9.4.2.23.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Suite(pub [u8; 4]);

impl Suite {
    const fn ieee(kind: u8) -> Self {
        Self([0x00, 0x0F, 0xAC, kind])
    }

    // Cipher suites, Table 9-188.
    pub const TKIP: Self = Self::ieee(2);
    pub const CCMP_128: Self = Self::ieee(4);
    pub const BIP_CMAC_128: Self = Self::ieee(6);
    pub const GROUP_ADDRESSED_TRAFFIC_NOT_ALLOWED: Self = Self::ieee(7);
    pub const GCMP_128: Self = Self::ieee(8);
    pub const GCMP_256: Self = Self::ieee(9);
    pub const CCMP_256: Self = Self::ieee(10);
    pub const BIP_GMAC_128: Self = Self::ieee(11);
    pub const BIP_GMAC_256: Self = Self::ieee(12);
    pub const BIP_CMAC_256: Self = Self::ieee(13);

    // AKM suites, Table 9-190.
    pub const AKM_8021X: Self = Self::ieee(1);
    pub const AKM_PSK: Self = Self::ieee(2);
    pub const AKM_FT_PSK: Self = Self::ieee(4);
    pub const AKM_PSK_SHA256: Self = Self::ieee(6);
    pub const AKM_SAE: Self = Self::ieee(8);
    pub const AKM_FT_SAE: Self = Self::ieee(9);
}

/// A list of suite selectors, four octets each.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Suites<'a>(&'a [u8]);

impl Suites<'_> {
    pub fn iter(&self) -> impl Iterator<Item = Suite> + '_ {
        self.0.as_chunks::<4>().0.iter().map(|&s| Suite(s))
    }

    pub fn contains(&self, suite: Suite) -> bool {
        self.iter().any(|s| s == suite)
    }

    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rsn<'a> {
    pub group_data: Option<Suite>,
    pub pairwise: Option<Suites<'a>>,
    pub akm: Option<Suites<'a>>,
    pub capabilities: Option<u16>,
    /// The PMKIDs, 16 octets each.
    pub pmkids: Option<&'a [u8]>,
    pub group_management: Option<Suite>,
}

const MFPR: u16 = 1 << 6;
const MFPC: u16 = 1 << 7;

impl<'a> Rsn<'a> {
    pub(super) fn parse(body: &'a [u8]) -> Result<Self, Refused> {
        let mut r = Reader::new(body);
        if r.u16().map_err(|_| Refused::ElementLength)? != 1 {
            return Err(Refused::RsnVersion);
        }
        let mut rsn = Self {
            group_data: None,
            pairwise: None,
            akm: None,
            capabilities: None,
            pmkids: None,
            group_management: None,
        };
        if r.is_empty() {
            return Ok(rsn);
        }
        rsn.group_data = Some(Suite(r.array()?));
        if r.is_empty() {
            return Ok(rsn);
        }
        rsn.pairwise = Some(Suites(counted(&mut r, 4)?));
        if r.is_empty() {
            return Ok(rsn);
        }
        rsn.akm = Some(Suites(counted(&mut r, 4)?));
        if r.is_empty() {
            return Ok(rsn);
        }
        let capabilities = r.u16()?;
        if capabilities & MFPR != 0 && capabilities & MFPC == 0 {
            return Err(Refused::MfpRequiredNotCapable);
        }
        rsn.capabilities = Some(capabilities);
        if r.is_empty() {
            return Ok(rsn);
        }
        rsn.pmkids = Some(counted(&mut r, 16)?);
        if r.is_empty() {
            return Ok(rsn);
        }
        rsn.group_management = Some(Suite(r.array()?));
        if !r.is_empty() {
            return Err(Refused::ElementLength);
        }
        Ok(rsn)
    }

    /// Management Frame Protection Capable, bit 7 of the RSN Capabilities field.
    pub fn mfp_capable(&self) -> bool {
        self.capabilities.is_some_and(|c| c & MFPC != 0)
    }

    /// Management Frame Protection Required, bit 6.
    pub fn mfp_required(&self) -> bool {
        self.capabilities.is_some_and(|c| c & MFPR != 0)
    }
}

/// A two-octet count, then that many items of `size` octets.
fn counted<'a>(r: &mut Reader<'a>, size: usize) -> Result<&'a [u8], Refused> {
    let count = usize::from(r.u16()?);
    let len = count.checked_mul(size).ok_or(Refused::RsnCount)?;
    r.take(len).map_err(|_| Refused::RsnCount)
}

/// The RSNXE: its Extended RSN Capabilities field, whose low four bits give its length less one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rsnx<'a>(&'a [u8]);

impl<'a> Rsnx<'a> {
    pub(super) fn parse(body: &'a [u8]) -> Result<Self, Refused> {
        let &first = body.first().ok_or(Refused::ElementLength)?;
        let len = usize::from(first & 0x0F).saturating_add(1);
        body.get(..len).map(Self).ok_or(Refused::ExtendedRsnLength)
    }

    pub fn bit(&self, n: usize) -> bool {
        self.0.get(n / 8).is_some_and(|octet| octet >> (n % 8) & 1 != 0)
    }

    /// SAE Hash-to-Element, bit 5 (Table 9-373).
    pub fn sae_h2e(&self) -> bool {
        self.bit(5)
    }
}
