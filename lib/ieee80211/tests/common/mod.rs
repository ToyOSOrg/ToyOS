#![allow(dead_code)]

use toyos_ieee80211::Address;

/// SplitMix64: a seeded stream, so a fuzz red names its iteration.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    pub fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }

    pub fn bytes(&mut self, max: usize) -> Vec<u8> {
        let len = self.below(max + 1);
        (0..len).map(|_| self.next() as u8).collect()
    }

    pub fn address(&mut self) -> Address {
        let mut a = [0; 6];
        a.iter_mut().for_each(|b| *b = self.next() as u8);
        a[0] &= !1;
        Address(a)
    }
}

pub const AP: Address = Address([0x02, 0x11, 0x22, 0x33, 0x44, 0x55]);
pub const STA: Address = Address([0x02, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE]);
pub const PEER: Address = Address([0x02, 0x01, 0x02, 0x03, 0x04, 0x05]);

/// A management frame's header: Frame Control's subtype, then DA, SA and BSSID.
pub fn management(subtype: u8, body: &[u8]) -> Vec<u8> {
    let mut f = vec![subtype << 4, 0, 0, 0];
    f.extend(STA.0);
    f.extend(AP.0);
    f.extend(AP.0);
    f.extend([0x10, 0x00]);
    f.extend(body);
    f
}

/// An element: its ID, its length, its body.
pub fn element(id: u8, body: &[u8]) -> Vec<u8> {
    let mut e = vec![id, body.len() as u8];
    e.extend(body);
    e
}

/// A Beacon body around `elements`.
pub fn beacon(elements: &[u8]) -> Vec<u8> {
    let mut b = vec![0; 8];
    b.extend(100u16.to_le_bytes());
    b.extend(0x0431u16.to_le_bytes());
    b.extend(elements);
    b
}

/// A QoS Data frame from the access point, its A-MSDU Present bit as given.
pub fn qos_from_ds(amsdu: bool, body: &[u8]) -> Vec<u8> {
    let mut f = vec![0x88, 0x02, 0, 0];
    f.extend(STA.0);
    f.extend(AP.0);
    f.extend(PEER.0);
    f.extend([0x20, 0x00]);
    f.extend([if amsdu { 0x80 } else { 0 }, 0]);
    f.extend(body);
    f
}

/// An A-MSDU subframe, padded to four octets unless it is the last.
pub fn subframe(da: Address, sa: Address, msdu: &[u8], last: bool) -> Vec<u8> {
    let mut s = Vec::new();
    s.extend(da.0);
    s.extend(sa.0);
    s.extend((msdu.len() as u16).to_be_bytes());
    s.extend(msdu);
    while !last && s.len() % 4 != 0 {
        s.push(0);
    }
    s
}

/// One record of a pcap file: its link type and the bytes captured.
pub struct Record {
    pub linktype: u32,
    pub bytes: Vec<u8>,
}

/// The records of a classic pcap file (draft-ietf-opsawg-pcap), either byte order.
pub fn pcap(file: &[u8]) -> Vec<Record> {
    let magic = u32::from_le_bytes(file[..4].try_into().unwrap());
    let le = match magic {
        0xa1b2_c3d4 | 0xa1b2_3c4d => true,
        0xd4c3_b2a1 | 0x4d3c_b2a1 => false,
        _ => panic!("not a pcap file: magic {magic:#x}"),
    };
    let word = |at: usize| {
        let b: [u8; 4] = file[at..at + 4].try_into().unwrap();
        if le { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) }
    };
    let linktype = word(20) & 0x0FFF_FFFF;
    let mut out = Vec::new();
    let mut at = 24;
    while at < file.len() {
        let len = word(at + 8) as usize;
        out.push(Record { linktype, bytes: file[at + 16..at + 16 + len].to_vec() });
        at += 16 + len;
    }
    out
}

/// The 802.11 frame a record holds, without a radiotap header (radiotap.org) and without an FCS:
/// a trailing four octets that are the CRC-32 of what precedes them are the FCS.
pub fn mpdu(record: &Record) -> Vec<u8> {
    let frame = match record.linktype & 0xFFFF {
        105 => &record.bytes[..],
        127 => {
            let len = u16::from_le_bytes([record.bytes[2], record.bytes[3]]) as usize;
            &record.bytes[len.min(record.bytes.len())..]
        }
        other => panic!("link type {other} is not 802.11"),
    };
    match frame.split_last_chunk::<4>() {
        Some((body, fcs)) if crc32(body) == u32::from_le_bytes(*fcs) => body.to_vec(),
        _ => frame.to_vec(),
    }
}

/// CRC-32 of IEEE 802.3 §3.2.9, which 802.11's FCS is (§9.2.4.8).
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}
