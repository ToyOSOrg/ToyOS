//! What `readlink` decides before the kernel is asked. It reads nothing but
//! what it is handed, so the host tests it (`toyos-libc-copies`).

/// The bytes a `readlink` buffer of `size` is read as, or `None` for a size it
/// refuses `EINVAL`: 0, as Linux's does, and one above `{SSIZE_MAX}`, whose
/// result POSIX leaves to the implementation: no slice is that long, and no
/// `ssize_t` counts it.
pub(crate) fn target_len(size: usize) -> Option<usize> {
    (size != 0 && isize::try_from(size).is_ok()).then_some(size)
}
