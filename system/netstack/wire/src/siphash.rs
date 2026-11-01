//! The keyed function behind every value a net crate keeps unpredictable, each purpose with its
//! own key.

pub use toyos_crypto::siphash::{siphash24, Key};

pub fn low32(key: &Key, data: &[u8]) -> u32 {
    let [a, b, c, d, ..] = siphash24(key, data).to_le_bytes();
    u32::from_le_bytes([a, b, c, d])
}
