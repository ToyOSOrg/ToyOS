//! A BSS as a scan heard it, what this station can join it by, and the choice among a
//! network's BSSes.
//!
//! **Joinable** is WPA2-Personal as `toyos-wlan-handshake` keys it: AKM 2 (PSK), CCMP-128 for
//! pairwise and group data, and management frame protection under BIP-CMAC-128 wherever the
//! access point offers it. An access point that requires protection under another cipher, or
//! demands a BSS membership selector this station does not meet (IEEE Std 802.11-2024
//! §9.4.2.3, Table 9-131), is refused by name.

use alloc::vec;
use alloc::vec::Vec;
use core::cmp::Ordering;

use toyos_ieee80211::Address;
use toyos_ieee80211::element::{self, Elements, Id, Rate, Suite, Suites};
use toyos_radio::{Bands, Channel, Signal};
use toyos_wifi::{Security, Ssid};

/// A signal at which a BSS is preferred over every weaker one whatever its band: about where
/// 5 GHz still carries its higher rates. Policy, not the standard's.
pub const STRONG: Signal = match Signal::new(-70) {
    Some(s) => s,
    None => panic!("-70 dBm is a signal"),
};

macro_rules! reasons {
    ($(#[$meta:meta])* $name:ident { $($variant:ident = $why:literal;)* }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum $name {
            $(#[doc = $why] $variant,)*
        }

        impl $name {
            pub const ALL: &'static [Self] = &[$(Self::$variant,)*];

            pub const fn reason(self) -> &'static str {
                match self {
                    $(Self::$variant => $why,)*
                }
            }
        }
    };
}

reasons! {
    /// Why this station cannot join a BSS.
    Unjoinable {
        Open = "an open or WEP network: this station joins WPA2-Personal networks";
        SaeOnly = "WPA3-Personal alone: this station does not run SAE yet";
        NoPsk = "no PSK AKM: not a WPA2-Personal network";
        Pairwise = "no CCMP-128 pairwise cipher";
        Group = "a group data cipher other than CCMP-128";
        Management = "management frame protection required under a cipher other than BIP-CMAC-128";
        Selector = "a BSS membership selector this station does not meet";
        Band = "a band the radio does not operate in";
    }
}

/// A BSS a scan heard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bss {
    pub bssid: Address,
    pub channel: Channel,
    pub signal: Signal,
    /// None for a hidden network's empty or zeroed SSID.
    pub ssid: Option<Ssid>,
    pub security: Security,
    /// Its RSNE whole, Element ID and Length included; empty for none.
    pub rsne: Vec<u8>,
    /// Its RSNXE whole, if it sent one.
    pub rsnxe: Option<Vec<u8>>,
    plan: Result<Plan, Unjoinable>,
}

/// What a join of a joinable BSS asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Plan {
    /// Whether this station says it is capable of management frame protection.
    protection: bool,
}

/// The element of `id` in `elements`, whole.
fn whole(elements: &[u8], id: u8) -> Option<Vec<u8>> {
    let e = element::iter(elements).filter_map(Result::ok).find(|e| e.id == Id::Base(id))?;
    let mut w = vec![id, u8::try_from(e.body.len()).ok()?];
    w.extend_from_slice(e.body);
    Some(w)
}

/// Whether a suite list holds `want`, an absent one meaning `default` (§9.4.2.23.1).
fn offers(suites: Option<Suites<'_>>, default: Suite, want: Suite) -> bool {
    suites.map_or(default == want, |s| s.contains(want))
}

impl Bss {
    /// A Beacon's or Probe Response's elements as a BSS: `None` for elements that do not parse,
    /// `Some(None)` for a BSS whose own channel is not `channel`.
    pub fn parse(bssid: Address, channel: Channel, signal: Signal, elements: &[u8]) -> Option<Option<Self>> {
        let e = Elements::parse(elements).ok()?;
        let own = [
            e.dsss_channel,
            e.ht_operation.map(|h| h.primary_channel),
            e.he_operation.and_then(|h| h.six_ghz).map(|s| s.primary_channel),
        ];
        if own.into_iter().flatten().any(|n| n != channel.number()) {
            return Some(None);
        }
        let ssid = e.ssid.filter(|s| s.iter().any(|&b| b != 0)).and_then(Ssid::new);
        let security = match e.rsn.map(|r| r.akm) {
            None => Security::Open,
            Some(akm) => match (
                offers(akm, Suite::AKM_8021X, Suite::AKM_PSK),
                offers(akm, Suite::AKM_8021X, Suite::AKM_SAE),
            ) {
                (true, false) => Security::Wpa2,
                (false, true) => Security::Wpa3,
                (true, true) => Security::Wpa2Wpa3,
                (false, false) => Security::Other,
            },
        };
        Some(Some(Self {
            bssid,
            channel,
            signal,
            ssid,
            security,
            rsne: whole(elements, 48).unwrap_or_default(),
            rsnxe: whole(elements, 244),
            plan: plan(&e, security),
        }))
    }

    /// Whether this station, over a radio of `bands`, can join it.
    pub fn joinable(&self, bands: Bands) -> Result<(), Unjoinable> {
        self.plan?;
        if !bands.has(self.channel.band()) {
            return Err(Unjoinable::Band);
        }
        Ok(())
    }

    /// The RSNE this station sends it: CCMP-128 and PSK, management frame protection capable
    /// where the access point's group management cipher is BIP-CMAC-128.
    pub fn own_rsne(&self) -> Vec<u8> {
        let protection = self.plan.is_ok_and(|p| p.protection);
        let mut rsne = vec![48, 20, 1, 0];
        rsne.extend_from_slice(&Suite::CCMP_128.0);
        rsne.extend_from_slice(&[1, 0]);
        rsne.extend_from_slice(&Suite::CCMP_128.0);
        rsne.extend_from_slice(&[1, 0]);
        rsne.extend_from_slice(&Suite::AKM_PSK.0);
        rsne.extend_from_slice(&[if protection { 0x80 } else { 0 }, 0]);
        rsne
    }
}

fn plan(e: &Elements<'_>, security: Security) -> Result<Plan, Unjoinable> {
    let Some(rsn) = e.rsn else { return Err(Unjoinable::Open) };
    match security {
        Security::Wpa2 | Security::Wpa2Wpa3 => {}
        Security::Wpa3 => return Err(Unjoinable::SaeOnly),
        Security::Open | Security::Other => return Err(Unjoinable::NoPsk),
    }
    if !offers(rsn.pairwise, Suite::CCMP_128, Suite::CCMP_128) {
        return Err(Unjoinable::Pairwise);
    }
    if rsn.group_data.unwrap_or(Suite::CCMP_128) != Suite::CCMP_128 {
        return Err(Unjoinable::Group);
    }
    let protection = rsn.group_management.unwrap_or(Suite::BIP_CMAC_128) == Suite::BIP_CMAC_128;
    if rsn.mfp_required() && !protection {
        return Err(Unjoinable::Management);
    }
    // Rates run to 54 Mb/s, 108 in units of 500 kb/s; a value above is a selector.
    let met = [Rate::HT_PHY, Rate::VHT_PHY, Rate::HE_PHY];
    if e.rates.is_some_and(|r| r.iter().any(|r| r.basic && r.value > 108 && !met.contains(&r.value))) {
        return Err(Unjoinable::Selector);
    }
    Ok(Plan { protection })
}

/// How two joinable BSSes of a network rank: one at least [`STRONG`] before any weaker; among
/// those, the higher band, then the stronger signal; among the weaker, the stronger signal.
fn rank(a: &Bss, b: &Bss) -> Ordering {
    let key = |x: &Bss| {
        let strong = x.signal >= STRONG;
        (strong, if strong { Some(x.channel.band()) } else { None }, x.signal)
    };
    key(a).cmp(&key(b))
}

/// The BSS of `ssid` to join over a radio of `bands`: `Err(None)` where none was heard, and
/// the strongest one's reason where none is joinable.
pub fn choose<'a>(heard: impl Iterator<Item = &'a Bss>, ssid: Ssid, bands: Bands) -> Result<&'a Bss, Option<Unjoinable>> {
    let mut best: Option<&Bss> = None;
    let mut strongest: Option<&Bss> = None;
    for bss in heard.filter(|b| b.ssid == Some(ssid)) {
        if strongest.is_none_or(|s| bss.signal > s.signal) {
            strongest = Some(bss);
        }
        if bss.joinable(bands).is_ok() && best.is_none_or(|b| rank(bss, b) == Ordering::Greater) {
            best = Some(bss);
        }
    }
    match (best, strongest) {
        (Some(bss), _) => Ok(bss),
        (None, None) => Err(None),
        (None, Some(s)) => Err(s.joinable(bands).err()),
    }
}
