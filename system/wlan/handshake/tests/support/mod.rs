//! An authenticator written from IEEE Std 802.11-2020 §12.7 for the tests to drive the
//! supplicant with, on ring's HMAC-SHA1 and the `aes` crate's block cipher rather than
//! toyos-crypto's. Each message is a [`Frame`] the test may change before it is sealed.

#![allow(dead_code)]

use aes::cipher::{BlockEncrypt, KeyInit, generic_array::GenericArray};
use ring::hmac;
use toyos_ieee80211::Address;

pub const AA: Address = Address([0x02, 0x00, 0x5e, 0x00, 0x00, 0x01]);
pub const SPA: Address = Address([0x02, 0x00, 0x5e, 0x00, 0x00, 0x02]);
pub const SNONCE: [u8; 32] = [0xc5; 32];

pub const ACK: u16 = 1 << 7;
pub const PAIRWISE: u16 = 1 << 3;
pub const INSTALL: u16 = 1 << 6;
pub const MIC: u16 = 1 << 8;
pub const SECURE: u16 = 1 << 9;
pub const ERROR: u16 = 1 << 10;
pub const REQUEST: u16 = 1 << 11;
pub const ENCRYPTED: u16 = 1 << 12;
pub const VERSION_2: u16 = 2;

/// An RSNE: PSK and CCMP-128, with RSN Capabilities `caps`.
pub fn rsne(caps: u16) -> Vec<u8> {
    let mut e = vec![
        48, 20, 1, 0, 0x00, 0x0f, 0xac, 4, 1, 0, 0x00, 0x0f, 0xac, 4, 1, 0, 0x00, 0x0f, 0xac, 2,
    ];
    e.extend_from_slice(&caps.to_le_bytes());
    e
}

pub const MFPC: u16 = 1 << 7;
pub const MFPR: u16 = 1 << 6;
/// An RSNXE announcing SAE hash-to-element, bit 5.
pub const RSNXE: [u8; 3] = [244, 1, 0x20];

pub fn kde(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut e = vec![
        0xdd,
        u8::try_from(4 + body.len()).unwrap(),
        0x00,
        0x0f,
        0xac,
        kind,
    ];
    e.extend_from_slice(body);
    e
}

pub fn gtk_kde(id: u8, key: &[u8]) -> Vec<u8> {
    let mut body = vec![id, 0];
    body.extend_from_slice(key);
    kde(1, &body)
}

pub fn igtk_kde(id: u16, ipn: u64, key: &[u8]) -> Vec<u8> {
    let mut body = id.to_le_bytes().to_vec();
    body.extend_from_slice(&ipn.to_le_bytes()[..6]);
    body.extend_from_slice(key);
    kde(9, &body)
}

pub fn hmac_sha1(key: &[u8], data: &[u8]) -> [u8; 20] {
    hmac::sign(
        &hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, key),
        data,
    )
    .as_ref()
    .try_into()
    .unwrap()
}

/// §12.7.1.2's PRF.
pub fn prf(key: &[u8], label: &[u8], data: &[u8], out: &mut [u8]) {
    for (i, chunk) in out.chunks_mut(20).enumerate() {
        let mut input = label.to_vec();
        input.push(0);
        input.extend_from_slice(data);
        input.push(i as u8);
        chunk.copy_from_slice(&hmac_sha1(key, &input)[..chunk.len()]);
    }
}

/// RFC 3394 §2.2.1, with §12.7.2's padding: 0xDD, then zeros, to a multiple of 8 and at least 16.
pub fn wrap(kek: &[u8; 16], plain: &[u8]) -> Vec<u8> {
    let mut p = plain.to_vec();
    if p.len() < 16 || !p.len().is_multiple_of(8) {
        p.push(0xdd);
        while p.len() < 16 || !p.len().is_multiple_of(8) {
            p.push(0);
        }
    }
    let cipher = aes::Aes128::new(GenericArray::from_slice(kek));
    let n = p.len() / 8;
    let mut a = [0xa6u8; 8];
    for j in 0..6 {
        for i in 1..=n {
            let mut b = [0u8; 16];
            b[..8].copy_from_slice(&a);
            b[8..].copy_from_slice(&p[8 * (i - 1)..8 * i]);
            let block = GenericArray::from_mut_slice(&mut b);
            cipher.encrypt_block(block);
            let t = (n * j + i) as u64;
            a.copy_from_slice(&(u64::from_be_bytes(b[..8].try_into().unwrap()) ^ t).to_be_bytes());
            p[8 * (i - 1)..8 * i].copy_from_slice(&b[8..]);
        }
    }
    let mut out = a.to_vec();
    out.extend_from_slice(&p);
    out
}

/// An EAPOL-Key frame's fields, sealed into bytes by [`Frame::seal`].
#[derive(Clone)]
pub struct Frame {
    pub version: u8,
    pub packet: u8,
    pub descriptor: u8,
    pub info: u16,
    pub key_length: u16,
    pub replay: u64,
    pub nonce: [u8; 32],
    pub rsc: [u8; 8],
    /// Key Data before it is wrapped, when `info` has [`ENCRYPTED`].
    pub data: Vec<u8>,
    /// Whether `data` goes into the frame as it is, wrapped or not.
    pub raw: bool,
    /// The key the MIC is computed under, when `info` has [`MIC`], and the key Key Data is
    /// wrapped under.
    pub kck: [u8; 16],
    pub kek: [u8; 16],
}

impl Frame {
    pub fn seal(&self) -> Vec<u8> {
        let data = if self.info & ENCRYPTED != 0 && !self.raw {
            wrap(&self.kek, &self.data)
        } else {
            self.data.clone()
        };
        let mut f = vec![self.version, self.packet, 0, 0, self.descriptor];
        f.extend_from_slice(&self.info.to_be_bytes());
        f.extend_from_slice(&self.key_length.to_be_bytes());
        f.extend_from_slice(&self.replay.to_be_bytes());
        f.extend_from_slice(&self.nonce);
        f.extend_from_slice(&[0; 16]);
        f.extend_from_slice(&self.rsc);
        f.extend_from_slice(&[0; 8 + 16]);
        f.extend_from_slice(&u16::try_from(data.len()).unwrap().to_be_bytes());
        f.extend_from_slice(&data);
        let body = u16::try_from(f.len() - 4).unwrap();
        f[2..4].copy_from_slice(&body.to_be_bytes());
        if self.info & MIC != 0 {
            let tag = hmac_sha1(&self.kck, &f);
            f[81..97].copy_from_slice(&tag[..16]);
        }
        f
    }
}

/// What a received supplicant frame says.
pub struct Received {
    pub version: u8,
    pub info: u16,
    pub key_length: u16,
    pub replay: u64,
    pub nonce: [u8; 32],
    pub data: Vec<u8>,
}

/// Parses a supplicant frame and checks its MIC under `kck`.
pub fn open(frame: &[u8], kck: &[u8; 16]) -> Received {
    assert_eq!(frame[1], 3, "an EAPOL-Key frame");
    assert_eq!(
        usize::from(u16::from_be_bytes([frame[2], frame[3]])),
        frame.len() - 4
    );
    assert_eq!(frame[4], 2);
    let mut zeroed = frame.to_vec();
    zeroed[81..97].fill(0);
    assert_eq!(
        frame[81..97],
        hmac_sha1(kck, &zeroed)[..16],
        "the supplicant's MIC verifies"
    );
    let data_len = usize::from(u16::from_be_bytes([frame[97], frame[98]]));
    assert_eq!(data_len, frame.len() - 99);
    Received {
        version: frame[0],
        info: u16::from_be_bytes([frame[5], frame[6]]),
        key_length: u16::from_be_bytes([frame[7], frame[8]]),
        replay: u64::from_be_bytes(frame[9..17].try_into().unwrap()),
        nonce: frame[17..49].try_into().unwrap(),
        data: frame[99..].to_vec(),
    }
}

/// The authenticator's side of one association.
pub struct Authenticator {
    pub pmk: [u8; 32],
    pub anonce: [u8; 32],
    pub replay: u64,
    pub rsne: Vec<u8>,
    pub rsnxe: Option<Vec<u8>>,
    pub gtk: (u8, [u8; 16]),
    pub gtk_rsc: u64,
    pub igtk: Option<(u16, u64, [u8; 16])>,
    /// KCK, KEK and TK, once message 2 arrived.
    pub ptk: Option<[u8; 48]>,
}

impl Authenticator {
    pub fn new(pmk: [u8; 32], rsne: Vec<u8>) -> Self {
        Self {
            pmk,
            anonce: [0xa5; 32],
            replay: 1,
            rsne,
            rsnxe: None,
            gtk: (1, [0x61; 16]),
            gtk_rsc: 0x0000_0102_0304_0506,
            igtk: None,
            ptk: None,
        }
    }

    pub fn kck(&self) -> [u8; 16] {
        self.ptk.unwrap()[..16].try_into().unwrap()
    }

    pub fn kek(&self) -> [u8; 16] {
        self.ptk.unwrap()[16..32].try_into().unwrap()
    }

    pub fn tk(&self) -> [u8; 16] {
        self.ptk.unwrap()[32..].try_into().unwrap()
    }

    fn base(&mut self, info: u16) -> Frame {
        self.replay += 1;
        Frame {
            version: 2,
            packet: 3,
            descriptor: 2,
            info: info | VERSION_2 | ACK,
            key_length: 16,
            replay: self.replay,
            nonce: [0; 32],
            rsc: [0; 8],
            data: Vec::new(),
            raw: false,
            kck: self
                .ptk
                .map(|p| p[..16].try_into().unwrap())
                .unwrap_or_default(),
            kek: self
                .ptk
                .map(|p| p[16..32].try_into().unwrap())
                .unwrap_or_default(),
        }
    }

    pub fn message_1(&mut self) -> Frame {
        let mut f = self.base(PAIRWISE);
        f.nonce = self.anonce;
        f
    }

    /// Takes message 2: checks its MIC under the PTK its SNonce gives, and returns it.
    pub fn message_2(&mut self, frame: &[u8]) -> Received {
        let snonce: [u8; 32] = frame[17..49].try_into().unwrap();
        let (a, b) = if AA.0 < SPA.0 {
            (AA.0, SPA.0)
        } else {
            (SPA.0, AA.0)
        };
        let (x, y) = if self.anonce < snonce {
            (self.anonce, snonce)
        } else {
            (snonce, self.anonce)
        };
        let data = [&a[..], &b, &x, &y].concat();
        let mut ptk = [0; 48];
        prf(&self.pmk, b"Pairwise key expansion", &data, &mut ptk);
        self.ptk = Some(ptk);
        open(frame, &self.kck())
    }

    pub fn group_data(&self) -> Vec<u8> {
        let mut data = gtk_kde(self.gtk.0, &self.gtk.1);
        if let Some((id, ipn, key)) = self.igtk {
            data.extend(igtk_kde(id, ipn, &key));
        }
        data
    }

    pub fn message_3(&mut self) -> Frame {
        let mut f = self.base(PAIRWISE | INSTALL | MIC | SECURE | ENCRYPTED);
        f.nonce = self.anonce;
        f.rsc[..8].copy_from_slice(&self.gtk_rsc.to_le_bytes());
        f.data = self.rsne.clone();
        if let Some(x) = &self.rsnxe {
            f.data.extend_from_slice(x);
        }
        f.data.extend(self.group_data());
        f
    }

    pub fn group_1(&mut self) -> Frame {
        let mut f = self.base(MIC | SECURE | ENCRYPTED);
        f.key_length = 0;
        f.rsc[..8].copy_from_slice(&self.gtk_rsc.to_le_bytes());
        f.data = self.group_data();
        f
    }
}
