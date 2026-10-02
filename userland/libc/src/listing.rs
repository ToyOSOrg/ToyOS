//! What `opendir`, `readdir` and `dladdr` make of the kernel's answers before
//! a caller sees them. It reads nothing but what it is handed, so the host
//! tests it (`toyos-libc-copies`).

use alloc::vec::Vec;

/// The whole answer of a call that writes nothing into a buffer too short for
/// it and answers the length it needs: asked again at that length until one
/// fits, since a listing or a module table can grow between two asks. Each ask
/// is longer than the last, and the kernel bounds what it answers.
pub(crate) fn whole<E>(mut ask: impl FnMut(&mut [u8]) -> Result<usize, E>) -> Result<Vec<u8>, E> {
    let mut buf = Vec::new();
    loop {
        let need = ask(&mut buf)?;
        if need <= buf.len() {
            buf.truncate(need);
            return Ok(buf);
        }
        buf.resize(need, 0);
    }
}

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
