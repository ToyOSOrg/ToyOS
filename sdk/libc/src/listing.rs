//! What `readdir` makes of a name before a caller sees it. It reads nothing
//! but what it is handed, so the host tests it (`toyos-libc-copies`).

/// The bytes of `struct dirent`'s `d_name`: `{NAME_MAX}` and its NUL.
pub(crate) const D_NAME: usize = 256;

/// `name` as `d_name` holds it, NUL-terminated, or `None` for a name too long
/// for it, which `readdir` answers `EOVERFLOW`.
pub(crate) fn d_name(name: &[u8]) -> Option<[u8; D_NAME]> {
    if name.len() >= D_NAME {
        return None;
    }
    let mut out = [0; D_NAME];
    out[..name.len()].copy_from_slice(name);
    Some(out)
}
