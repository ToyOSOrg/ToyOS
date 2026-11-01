//! CRC-32C (Castagnoli, polynomial 0x1EDC6F41), by the CPU's own instruction:
//! SSE4.2's `CRC32` on x86-64, FEAT_CRC32's `CRC32C` on AArch64. Every
//! machine ToyOS runs on and every host that builds or tests it has one, so
//! nothing computes it any other way.

#[cfg_attr(target_arch = "x86_64", path = "x86_64.rs")]
#[cfg_attr(target_arch = "aarch64", path = "aarch64.rs")]
mod arch;

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
compile_error!("bcachefs computes CRC-32C with the CPU's instruction, and names one for x86-64 and AArch64 alone");

/// Compute CRC-32c over a byte slice.
pub fn crc32c(data: &[u8]) -> u32 {
    arch::update(0xFFFF_FFFF, data) ^ 0xFFFF_FFFF
}

/// The same register with no initial or final inversion: bcachefs's data
/// checksum type 5, whose value over zeros is 0.
pub fn crc32c_zero(data: &[u8]) -> u32 {
    arch::update(0, data)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The oracle: the reflected polynomial a bit at a time, sharing nothing
    /// with the instruction but the register's definition.
    fn reference(mut crc: u32, data: &[u8]) -> u32 {
        for &byte in data {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = if crc & 1 != 0 { (crc >> 1) ^ 0x82F6_3B78 } else { crc >> 1 };
            }
        }
        crc
    }

    /// RFC 3720's own examples (Appendix B.4), and the catalogue's check
    /// value over "123456789".
    #[test]
    fn rfc_3720_vectors() {
        let ascending: Vec<u8> = (0..32).collect();
        let descending: Vec<u8> = (0..32).rev().collect();
        #[rustfmt::skip]
        let read_pdu: [u8; 48] = [
            0x01, 0xc0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x14, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x14, 0x00, 0x00, 0x00, 0x18,
            0x28, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        let vectors: [(&[u8], u32); 6] = [
            (&[0; 32], 0x8A91_36AA),
            (&[0xFF; 32], 0x62A8_AB43),
            (&ascending, 0x46DD_794E),
            (&descending, 0x113F_DB5C),
            (&read_pdu, 0xD996_3A56),
            (b"123456789", 0xE306_9283),
        ];
        for (data, want) in vectors {
            assert_eq!(crc32c(data), want, "{data:02x?}");
            assert_eq!(reference(!0, data) ^ !0, want, "the oracle on {data:02x?}");
        }
        assert_eq!(crc32c(b""), 0);
    }

    #[test]
    fn the_uninverted_register_has_its_own_check_value() {
        assert_eq!(crc32c_zero(b"123456789"), 0x58E3_FA20);
        assert_eq!(crc32c_zero(&[0; 4096]), 0);
    }

    /// Every length to past two words, at every alignment a word can have,
    /// and whole nodes, against the oracle: the instruction's word steps and
    /// its byte tail agree with a bit at a time.
    #[test]
    fn the_instruction_is_the_oracle_at_every_length_and_alignment() {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let bytes: Vec<u8> = (0..16_384 + 8)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect();
        for at in 0..8 {
            for len in (0..=24).chain([4088, 4096, 16_384]) {
                let data = &bytes[at..at + len];
                assert_eq!(crc32c(data), reference(!0, data) ^ !0, "{len} bytes at {at}");
                assert_eq!(crc32c_zero(data), reference(0, data), "{len} bytes at {at}, uninverted");
            }
        }
    }
}
