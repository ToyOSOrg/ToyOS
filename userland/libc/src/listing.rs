//! The kernel's answer to `SYS_READDIR`, one entry at a time: a kind byte (1
//! for a file, 2 for a directory), the name, a NUL, and the size as eight
//! bytes, little-endian. It reads nothing but what it is handed, so the host
//! tests it (`toyos-libc-copies`).

/// One entry of a listing.
pub(crate) struct Entry<'a> {
    pub(crate) is_dir: bool,
    pub(crate) name: &'a [u8],
}

/// The entry at `*pos` in `answer`, with `*pos` moved past it; `None` once
/// `*pos` is at the end. An answer of any other shape is a kernel that broke
/// its ABI, and panics.
pub(crate) fn next<'a>(answer: &'a [u8], pos: &mut usize) -> Option<Entry<'a>> {
    let rest = answer.get(*pos..).filter(|r| !r.is_empty())?;
    let is_dir = match rest[0] {
        1 => false,
        2 => true,
        kind => panic!("SYS_READDIR answered an entry of kind {kind}"),
    };
    let len = rest[1..].iter().position(|&b| b == 0).expect("SYS_READDIR answered a name with no NUL");
    let end = 1 + len + 1 + 8;
    assert!(rest.len() >= end, "SYS_READDIR answered an entry with no size");
    *pos += end;
    Some(Entry { is_dir, name: &rest[1..1 + len] })
}
