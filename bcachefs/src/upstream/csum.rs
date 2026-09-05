//! The three unkeyed checksums a bcachefs superblock may name, in upstream's
//! seeding: `bch2_checksum_init`/`_update`/`_final` seed crc32c and crc64 with
//! 0 and do not invert, and xxhash is XXH64 seeded 0.

use super::UpstreamError;

/// The checksum types `BCH_CSUM_TYPES()` numbers. A type this crate cannot
/// compute is refused by name at the boundary rather than skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CsumType {
    None,
    Crc32cNonzero,
    Crc64Nonzero,
    Crc32c,
    Crc64,
    Xxhash,
}

impl CsumType {
    pub fn from_disk(raw: u64) -> Result<Self, UpstreamError> {
        match raw {
            0 => Ok(Self::None),
            1 => Ok(Self::Crc32cNonzero),
            2 => Ok(Self::Crc64Nonzero),
            3 => Err(UpstreamError::Refused("checksum type chacha20_poly1305_80: encryption")),
            4 => Err(UpstreamError::Refused("checksum type chacha20_poly1305_128: encryption")),
            5 => Ok(Self::Crc32c),
            6 => Ok(Self::Crc64),
            7 => Ok(Self::Xxhash),
            _ => Err(UpstreamError::Refused("checksum type is not one this format defines")),
        }
    }

    /// The 128-bit `bch_csum` upstream stores: the digest in `lo`, `hi` zero
    /// for every type here.
    pub fn digest(self, data: &[u8]) -> (u64, u64) {
        let lo = match self {
            Self::None => 0,
            Self::Crc32c => crc32c_update(0, data) as u64,
            Self::Crc32cNonzero => (crc32c_update(u32::MAX, data) ^ u32::MAX) as u64,
            Self::Crc64 => crc64_be(0, data),
            Self::Crc64Nonzero => crc64_be(u64::MAX, data) ^ u64::MAX,
            Self::Xxhash => xxh64(data, 0),
        };
        (lo, 0)
    }

    /// True when `data` carries the checksum `stored` claims.
    ///
    /// `CsumType::None` verifies everything, which is upstream's behaviour and
    /// is why a mount refuses `none` metadata rather than trusting this.
    pub fn verify(self, data: &[u8], stored: (u64, u64)) -> bool {
        self.digest(data) == stored
    }
}

const CRC32C_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0u32;
    while i < 256 {
        let mut crc = i;
        let mut j = 0;
        while j < 8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0x82F6_3B78 } else { crc >> 1 };
            j += 1;
        }
        table[i as usize] = crc;
        i += 1;
    }
    table
};

/// Linux's `crc32c(seed, data, len)`: the Castagnoli update with neither the
/// pre- nor the post-inversion the standalone CRC-32C carries.
pub fn crc32c_update(mut crc: u32, data: &[u8]) -> u32 {
    for &byte in data {
        crc = (crc >> 8) ^ CRC32C_TABLE[((crc ^ byte as u32) & 0xFF) as usize];
    }
    crc
}

const CRC64_TABLE: [u64; 256] = {
    let mut table = [0u64; 256];
    let mut i = 0u64;
    while i < 256 {
        let mut crc = i << 56;
        let mut j = 0;
        while j < 8 {
            crc = if crc >> 63 != 0 {
                (crc << 1) ^ 0x42F0_E1EB_A9EA_3693
            } else {
                crc << 1
            };
            j += 1;
        }
        table[i as usize] = crc;
        i += 1;
    }
    table
};

/// Linux's `crc64_be`: CRC-64/ECMA-182, most significant bit first.
pub fn crc64_be(mut crc: u64, data: &[u8]) -> u64 {
    for &byte in data {
        crc = CRC64_TABLE[(((crc >> 56) ^ byte as u64) & 0xFF) as usize] ^ (crc << 8);
    }
    crc
}

const XXH_P1: u64 = 0x9E37_79B1_85EB_CA87;
const XXH_P2: u64 = 0xC2B2_AE3D_27D4_EB4F;
const XXH_P3: u64 = 0x1656_67B1_9E37_79F9;
const XXH_P4: u64 = 0x85EB_CA77_C2B2_AE63;
const XXH_P5: u64 = 0x27D4_EB2F_1656_67C5;

fn xxh_round(acc: u64, input: u64) -> u64 {
    acc.wrapping_add(input.wrapping_mul(XXH_P2)).rotate_left(31).wrapping_mul(XXH_P1)
}

fn xxh_merge(acc: u64, val: u64) -> u64 {
    (acc ^ xxh_round(0, val)).wrapping_mul(XXH_P1).wrapping_add(XXH_P4)
}

/// XXH64, the one-shot form; `xxh64_update` over a whole buffer is this.
pub fn xxh64(data: &[u8], seed: u64) -> u64 {
    let mut rest = data;
    let mut hash;

    if data.len() >= 32 {
        let (mut v1, mut v2, mut v3, mut v4) = (
            seed.wrapping_add(XXH_P1).wrapping_add(XXH_P2),
            seed.wrapping_add(XXH_P2),
            seed,
            seed.wrapping_sub(XXH_P1),
        );
        while rest.len() >= 32 {
            v1 = xxh_round(v1, le64(rest, 0));
            v2 = xxh_round(v2, le64(rest, 8));
            v3 = xxh_round(v3, le64(rest, 16));
            v4 = xxh_round(v4, le64(rest, 24));
            rest = &rest[32..];
        }
        hash = v1
            .rotate_left(1)
            .wrapping_add(v2.rotate_left(7))
            .wrapping_add(v3.rotate_left(12))
            .wrapping_add(v4.rotate_left(18));
        hash = xxh_merge(hash, v1);
        hash = xxh_merge(hash, v2);
        hash = xxh_merge(hash, v3);
        hash = xxh_merge(hash, v4);
    } else {
        hash = seed.wrapping_add(XXH_P5);
    }

    hash = hash.wrapping_add(data.len() as u64);

    while rest.len() >= 8 {
        hash = (hash ^ xxh_round(0, le64(rest, 0))).rotate_left(27).wrapping_mul(XXH_P1);
        hash = hash.wrapping_add(XXH_P4);
        rest = &rest[8..];
    }
    if rest.len() >= 4 {
        let word = u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]) as u64;
        hash = (hash ^ word.wrapping_mul(XXH_P1)).rotate_left(23).wrapping_mul(XXH_P2);
        hash = hash.wrapping_add(XXH_P3);
        rest = &rest[4..];
    }
    for &byte in rest {
        hash = (hash ^ (byte as u64).wrapping_mul(XXH_P5)).rotate_left(11).wrapping_mul(XXH_P1);
    }

    hash ^= hash >> 33;
    hash = hash.wrapping_mul(XXH_P2);
    hash ^= hash >> 29;
    hash = hash.wrapping_mul(XXH_P3);
    hash ^= hash >> 32;
    hash
}

fn le64(buf: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(buf[off..off + 8].try_into().expect("an 8-byte window"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The check values the three algorithms' own specifications publish, and
    /// the seedings upstream picks out of them.
    #[test]
    fn published_check_values() {
        assert_eq!(crc32c_update(u32::MAX, b"123456789") ^ u32::MAX, 0xE306_9283);
        assert_eq!(crc64_be(0, b"123456789"), 0x6c40_df5f_0b49_7347);
        assert_eq!(xxh64(b"", 0), 0xEF46_DB37_51D8_E999);
    }

    /// XXH64's three tails — the 32-byte stripes, the 8- and 4-byte words and
    /// the loose bytes — are separate branches, so a length past each is what
    /// covers them.
    #[test]
    fn xxh64_covers_every_tail() {
        let data: Vec<u8> = (0u8..=200).cycle().take(201).collect();
        for len in [0usize, 1, 3, 4, 7, 8, 31, 32, 33, 63, 64, 200] {
            let a = xxh64(&data[..len], 0);
            let b = xxh64(&data[..len], 0);
            assert_eq!(a, b, "xxh64 is not a function at length {len}");
        }
        assert_ne!(xxh64(&data[..32], 0), xxh64(&data[..33], 0));
    }

    /// Every checksum type puts its digest in `lo` and leaves `hi` zero, and
    /// the two encrypted types are refused by name rather than numbered.
    #[test]
    fn encryption_csum_types_are_refused_by_name() {
        for raw in [3u64, 4] {
            let err = CsumType::from_disk(raw).expect_err("an encrypted volume must be refused");
            assert!(
                matches!(err, UpstreamError::Refused(reason) if reason.contains("encryption")),
                "{err:?} does not name encryption"
            );
        }
        assert!(CsumType::from_disk(8).is_err(), "an undefined checksum type must be refused");
        assert_eq!(CsumType::Crc32c.digest(b"abc").1, 0);
    }
}
