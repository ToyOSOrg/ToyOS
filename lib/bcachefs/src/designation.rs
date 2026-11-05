//! An explicit permission to format a partition, separate from bcachefs's
//! layout. Mkfs overwrites this first block, consuming the designation.

use toyos_blockdev::BLOCK;

const MAGIC: [u8; 16] = *b"TOYOS-FORMAT-ME\0";

pub fn designation(blocks: u64) -> [u8; BLOCK] {
    let mut bytes = [0; BLOCK];
    bytes[..16].copy_from_slice(&MAGIC);
    bytes[16..24].copy_from_slice(&blocks.to_le_bytes());
    bytes
}

pub fn designated(bytes: &[u8]) -> Option<u64> {
    if bytes.get(..16)? != MAGIC { return None; }
    Some(u64::from_le_bytes(bytes.get(16..24)?.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_designation_names_exactly_its_partition_size() {
        for blocks in [0, 1, 65536, u64::MAX] {
            assert_eq!(designated(&designation(blocks)), Some(blocks));
        }
        assert_eq!(designated(&designation(7)[..23]), None);
        assert_eq!(designated(&[0; BLOCK]), None);
        assert!(designation(7)[24..].iter().all(|&byte| byte == 0));
    }
}
