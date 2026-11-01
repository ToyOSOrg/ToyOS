//! Data frame bodies: the CCMP header (§12.5.2.2), A-MSDU (§9.3.2.2), and translation between an
//! MSDU and an Ethernet II frame under RFC 1042's LLC/SNAP header alone, every EtherType alike:
//! §5.1.4's bridge-tunnel translation is not implemented, and its OUI is refused as any other.

use crate::frame::Data;
use crate::read::Reader;
use crate::{Address, Refused, Seq};

/// The CCMP header that stays in front of a frame body its receiver decrypted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CcmpHeader {
    /// The 48-bit packet number.
    pub pn: u64,
    pub key_id: u8,
}

impl CcmpHeader {
    pub const LEN: usize = 8;

    /// The header, and the body after it.
    pub fn split(body: &[u8]) -> Result<(Self, &[u8]), Refused> {
        let mut r = Reader::new(body);
        let [pn0, pn1, _, key, pn2, pn3, pn4, pn5] = r.array()?;
        if key & 1 << 5 == 0 {
            return Err(Refused::ExtendedIvClear);
        }
        let pn = u64::from_le_bytes([pn0, pn1, pn2, pn3, pn4, pn5, 0, 0]);
        Ok((Self { pn, key_id: key >> 6 }, r.rest()))
    }
}

/// One MSDU and the addresses it travels between.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Msdu<'a> {
    pub da: Address,
    pub sa: Address,
    pub payload: &'a [u8],
}

/// The MSDUs `frame` carries in `body`, its plaintext: one, or an A-MSDU's subframes in order.
///
/// An A-MSDU subframe header is an IEEE 802.3 header, its Length big-endian, and every subframe
/// but the last is padded to a multiple of four octets.
pub fn msdus<'a>(frame: &Data<'_>, body: &'a [u8]) -> Result<Msdus<'a>, Refused> {
    if frame.null {
        return Ok(Msdus(Inner::One(None)));
    }
    if !frame.qos.is_some_and(|q| q.amsdu()) {
        return Ok(Msdus(Inner::One(Some(Msdu { da: frame.da(), sa: frame.sa(), payload: body }))));
    }
    // The A-MSDU Present bit is outside CCMP's protection unless both peers negotiated SPP
    // A-MSDU, so a first subframe whose destination is an LLC/SNAP header is a frame an attacker
    // flipped the bit on, which §10.11 forbids (Vanhoef, "Fragment and Forge", USENIX Security
    // 2021, §3.2).
    if body.starts_with(&SNAP) {
        return Err(Refused::SnapHeaderAsAddress);
    }
    Ok(Msdus(Inner::Subframes(Reader::new(body))))
}

pub struct Msdus<'a>(Inner<'a>);

enum Inner<'a> {
    One(Option<Msdu<'a>>),
    Subframes(Reader<'a>),
}

impl<'a> Iterator for Msdus<'a> {
    type Item = Result<Msdu<'a>, Refused>;

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.0 {
            Inner::One(one) => one.take().map(Ok),
            Inner::Subframes(r) if r.is_empty() => None,
            Inner::Subframes(r) => {
                let next = subframe(r);
                if next.is_err() {
                    *r = Reader::new(&[]);
                }
                Some(next)
            }
        }
    }
}

fn subframe<'a>(r: &mut Reader<'a>) -> Result<Msdu<'a>, Refused> {
    let da = r.address()?;
    let sa = r.address()?;
    let len = u16::from_be_bytes(r.array()?);
    let payload = r.take(usize::from(len)).map_err(|_| Refused::SubframeLength)?;
    if !r.is_empty() {
        let pad = usize::from(len.wrapping_add(14).wrapping_neg() & 3);
        r.take(pad).map_err(|_| Refused::SubframePadding)?;
        if r.is_empty() {
            return Err(Refused::SubframePadding);
        }
    }
    Ok(Msdu { da, sa, payload })
}

/// An Ethernet II frame's header fields, and its payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ethernet<'a> {
    pub dst: Address,
    pub src: Address,
    pub ethertype: u16,
    pub payload: &'a [u8],
}

const SNAP: [u8; 6] = [0xAA, 0xAA, 0x03, 0x00, 0x00, 0x00];
/// The least type: IEEE Std 802.3-2022 §3.2.6 reads a Length/Type value of at most 1500 as a
/// length and one of at least 1536 (0x0600) as a type.
const MIN_ETHERTYPE: u16 = 0x0600;

pub const LLC_SNAP_LEN: usize = 8;

impl<'a> Ethernet<'a> {
    pub fn from_msdu(msdu: Msdu<'a>) -> Result<Self, Refused> {
        let mut r = Reader::new(msdu.payload);
        let header: [u8; 6] = r.array()?;
        let ethertype = u16::from_be_bytes(r.array()?);
        let [llc0, llc1, llc2, ..] = header;
        if [llc0, llc1, llc2] != [0xAA, 0xAA, 0x03] {
            return Err(Refused::NotSnap);
        }
        if header != SNAP {
            return Err(Refused::UnknownOui);
        }
        if ethertype < MIN_ETHERTYPE {
            return Err(Refused::LengthField);
        }
        Ok(Self { dst: msdu.da, src: msdu.sa, ethertype, payload: r.rest() })
    }
}

/// The LLC/SNAP header an MSDU carrying `ethertype` starts with.
pub fn llc_snap(ethertype: u16) -> Result<[u8; LLC_SNAP_LEN], Refused> {
    if ethertype < MIN_ETHERTYPE {
        return Err(Refused::LengthField);
    }
    let [oui0, oui1, oui2, oui3, oui4, oui5] = SNAP;
    let [t0, t1] = ethertype.to_be_bytes();
    Ok([oui0, oui1, oui2, oui3, oui4, oui5, t0, t1])
}

/// The header of a QoS Data frame a station sends its access point: To DS, Address 1 the BSSID,
/// Address 2 the source and Address 3 the destination (Table 9-60).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToDs {
    pub bssid: Address,
    pub sa: Address,
    pub da: Address,
    pub seq: Seq,
    pub tid: u8,
    pub protected: bool,
}

impl ToDs {
    pub const LEN: usize = 26;

    pub fn header(&self) -> [u8; Self::LEN] {
        let mut out = [0; Self::LEN];
        let protected = if self.protected { 0x40 } else { 0 };
        let fields = [0x88, 0x01 | protected, 0, 0]
            .into_iter()
            .chain(self.bssid.0)
            .chain(self.sa.0)
            .chain(self.da.0)
            .chain((self.seq.get() << 4).to_le_bytes())
            .chain(u16::from(self.tid & 0x0F).to_le_bytes());
        for (slot, byte) in out.iter_mut().zip(fields) {
            *slot = byte;
        }
        out
    }
}
