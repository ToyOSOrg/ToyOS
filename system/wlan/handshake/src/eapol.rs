//! EAPOL-Key frames (IEEE Std 802.1X-2020 §11.3; IEEE Std 802.11-2020 §12.7.2): the
//! EAPOL PDU an EtherType 0x888E MSDU carries, from its Protocol Version octet to its Key Data.

use alloc::vec::Vec;
use toyos_crypto::{ct, wpa2};

use crate::Refused;

/// EAPOL Packet Type EAPOL-Key (IEEE Std 802.1X-2020 §11.3).
const PACKET_KEY: u8 = 3;
/// Descriptor Type of the IEEE 802.11 key descriptor (§12.7.2).
const DESCRIPTOR_IEEE80211: u8 = 2;
/// The EAPOL header: Protocol Version, Packet Type and Packet Body Length.
const HEADER: usize = 4;
/// From Descriptor Type through Key Data Length, with AKM 2's 16-octet Key MIC.
const FIXED_BODY: usize = 95;
/// Where the Key MIC lies in the PDU.
const MIC_AT: usize = 81;
pub(crate) const MIC_LEN: usize = 16;

/// Key Information bits (§12.7.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Info(pub(crate) u16);

impl Info {
    /// Key Descriptor Version 2: HMAC-SHA1-128 MICs, AES key wrap of Key Data.
    pub(crate) const VERSION_2: u16 = 2;
    const VERSION: u16 = 0b111;
    pub(crate) const PAIRWISE: u16 = 1 << 3;
    pub(crate) const INSTALL: u16 = 1 << 6;
    pub(crate) const ACK: u16 = 1 << 7;
    pub(crate) const MIC: u16 = 1 << 8;
    pub(crate) const SECURE: u16 = 1 << 9;
    pub(crate) const ERROR: u16 = 1 << 10;
    pub(crate) const REQUEST: u16 = 1 << 11;
    pub(crate) const ENCRYPTED: u16 = 1 << 12;

    pub(crate) const fn has(self, bit: u16) -> bool {
        self.0 & bit != 0
    }
}

/// An EAPOL-Key frame, parsed in place.
pub(crate) struct Key<'a> {
    /// The EAPOL Protocol Version, which the reply echoes.
    pub(crate) version: u8,
    pub(crate) info: Info,
    pub(crate) key_length: u16,
    pub(crate) replay: u64,
    pub(crate) nonce: [u8; 32],
    pub(crate) rsc: [u8; 8],
    pub(crate) data: &'a [u8],
    mic: [u8; MIC_LEN],
    /// The PDU through its Key Data, without any octet past its Packet Body Length.
    pdu: &'a [u8],
}

fn take<const N: usize>(rest: &mut &[u8]) -> Result<[u8; N], Refused> {
    let (head, tail) = rest.split_first_chunk::<N>().ok_or(Refused::Truncated)?;
    *rest = tail;
    Ok(*head)
}

impl<'a> Key<'a> {
    /// Octets past the Packet Body Length are not the PDU's and are ignored.
    pub(crate) fn parse(frame: &'a [u8]) -> Result<Self, Refused> {
        let mut r = frame;
        let [version, packet] = take(&mut r)?;
        let body = usize::from(u16::from_be_bytes(take(&mut r)?));
        if !(1..=3).contains(&version) {
            return Err(Refused::ProtocolVersion);
        }
        if packet != PACKET_KEY {
            return Err(Refused::PacketType);
        }
        if body < FIXED_BODY {
            return Err(Refused::Truncated);
        }
        let mut r = r.get(..body).ok_or(Refused::BodyLength)?;
        let pdu = frame
            .get(..HEADER.saturating_add(body))
            .ok_or(Refused::BodyLength)?;
        let [descriptor] = take(&mut r)?;
        if descriptor != DESCRIPTOR_IEEE80211 {
            return Err(Refused::DescriptorType);
        }
        let info = Info(u16::from_be_bytes(take(&mut r)?));
        if info.0 & Info::VERSION != Info::VERSION_2 {
            return Err(Refused::DescriptorVersion);
        }
        let key_length = u16::from_be_bytes(take(&mut r)?);
        let replay = u64::from_be_bytes(take(&mut r)?);
        let nonce = take(&mut r)?;
        let _iv: [u8; 16] = take(&mut r)?;
        let rsc = take(&mut r)?;
        let _reserved: [u8; 8] = take(&mut r)?;
        let mic = take(&mut r)?;
        let data_len = usize::from(u16::from_be_bytes(take(&mut r)?));
        if data_len != r.len() {
            return Err(Refused::KeyDataLength);
        }
        Ok(Self {
            version,
            info,
            key_length,
            replay,
            nonce,
            rsc,
            data: r,
            mic,
            pdu,
        })
    }

    /// Whether the Key MIC is HMAC-SHA1-128 under `kck` of the PDU with its MIC zeroed (§12.7.2).
    pub(crate) fn mic_verifies(&self, kck: &[u8]) -> bool {
        let mut zeroed = self.pdu.to_vec();
        zeroed
            .get_mut(MIC_AT..MIC_AT.saturating_add(MIC_LEN))
            .expect("a PDU parsed has its whole fixed body")
            .fill(0);
        ct::equal(&mic(kck, &zeroed), &self.mic)
    }
}

fn mic(kck: &[u8], pdu: &[u8]) -> [u8; MIC_LEN] {
    let tag = wpa2::hmac_sha1(kck, &[pdu]).expect("an EAPOL PDU is far inside HMAC-SHA1's bound");
    *tag.first_chunk().expect("HMAC-SHA1 gives 20 octets")
}

/// A frame the supplicant sends, its Key MIC set under `kck`. Its Key Data is sent in the clear,
/// so it holds no secret.
pub(crate) struct Reply<'a> {
    pub(crate) version: u8,
    pub(crate) info: u16,
    pub(crate) replay: u64,
    pub(crate) nonce: &'a [u8; 32],
    pub(crate) data: &'a [u8],
}

impl Reply<'_> {
    pub(crate) fn seal(&self, kck: &[u8]) -> Vec<u8> {
        let data_len = u16::try_from(self.data.len())
            .expect("a reply's Key Data is the station's own RSNE and RSNXE");
        let body =
            u16::try_from(FIXED_BODY.saturating_add(self.data.len())).expect("as its Key Data");
        let mut pdu = Vec::with_capacity(HEADER.saturating_add(usize::from(body)));
        pdu.extend_from_slice(&[self.version, PACKET_KEY]);
        pdu.extend_from_slice(&body.to_be_bytes());
        pdu.push(DESCRIPTOR_IEEE80211);
        pdu.extend_from_slice(&(self.info | Info::VERSION_2 | Info::MIC).to_be_bytes());
        // Key Length is 0 in every frame a supplicant sends (§12.7.6.3, §12.7.6.5, §12.7.7.3).
        pdu.extend_from_slice(&0u16.to_be_bytes());
        pdu.extend_from_slice(&self.replay.to_be_bytes());
        pdu.extend_from_slice(self.nonce);
        // EAPOL-Key IV, Key RSC, Reserved and the Key MIC, which is zero while it is computed.
        pdu.extend_from_slice(&[0; 16 + 8 + 8 + MIC_LEN]);
        pdu.extend_from_slice(&data_len.to_be_bytes());
        pdu.extend_from_slice(self.data);
        let tag = mic(kck, &pdu);
        pdu.get_mut(MIC_AT..MIC_AT.saturating_add(MIC_LEN))
            .expect("a reply has its whole fixed body")
            .copy_from_slice(&tag);
        pdu
    }
}
