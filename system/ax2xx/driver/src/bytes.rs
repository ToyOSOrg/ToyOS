//! Little-endian fields at an offset, `None` where the bytes run short.

pub(crate) fn array<const N: usize>(bytes: &[u8], at: usize) -> Option<[u8; N]> {
    bytes.get(at..at.checked_add(N)?)?.try_into().ok()
}

pub(crate) fn u8_at(bytes: &[u8], at: usize) -> Option<u8> {
    bytes.get(at).copied()
}

pub(crate) fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    array(bytes, at).map(u16::from_le_bytes)
}

pub(crate) fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    array(bytes, at).map(u32::from_le_bytes)
}

pub(crate) fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
    array(bytes, at).map(u64::from_le_bytes)
}

/// `bytes` as consecutive little-endian words, `None` unless it is whole words.
pub(crate) fn words(bytes: &[u8]) -> Option<impl Iterator<Item = u32> + '_> {
    let (words, rest) = bytes.as_chunks::<4>();
    rest.is_empty().then(|| words.iter().map(|w| u32::from_le_bytes(*w)))
}
