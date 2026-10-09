//! The socket options libc keeps a value for, and how an option's value
//! crosses `setsockopt` and `getsockopt`: each an `int`, refused and truncated
//! as Linux does, whose option numbers these are. It reads nothing but what it
//! is handed, so the host tests it (`toyos-libc-copies`) against the host's
//! own two calls.

use core::mem::size_of;

pub(crate) const IPPROTO_TCP: i32 = 6;
pub(crate) const SOL_SOCKET: i32 = 1;
pub(crate) const SO_BROADCAST: i32 = 6;
pub(crate) const TCP_NODELAY: i32 = 1;

/// An option a socket keeps a value for.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Kept {
    NoDelay,
    Broadcast,
}

/// The kept option `optname` names at `level`.
pub(crate) fn kept(level: i32, optname: i32) -> Option<Kept> {
    match (level, optname) {
        (IPPROTO_TCP, TCP_NODELAY) => Some(Kept::NoDelay),
        (SOL_SOCKET, SO_BROADCAST) => Some(Kept::Broadcast),
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Refusal {
    /// A value shorter than an `int`: `EINVAL`.
    Short,
    /// A null pointer where bytes are read or written: `EFAULT`.
    Fault,
}

/// What a setter is told: whether the `int` at `optval` is non-zero. The
/// length is judged before the pointer is.
///
/// # Safety
/// `optval` is null or `optlen` readable bytes.
pub(crate) unsafe fn switch(optval: *const u8, optlen: u32) -> Result<bool, Refusal> {
    if (optlen as usize) < size_of::<i32>() {
        return Err(Refusal::Short);
    }
    if optval.is_null() {
        return Err(Refusal::Fault);
    }
    Ok(optval.cast::<i32>().read_unaligned() != 0)
}

/// Answers a getter `value`: as many of its bytes as `*optlen` holds, at most
/// an `int`'s, and that count written back. A buffer no byte goes into may be
/// null.
///
/// # Safety
/// `optlen` is null or a writable length, and `optval` null or `*optlen`
/// writable bytes.
pub(crate) unsafe fn answer(value: i32, optval: *mut u8, optlen: *mut u32) -> Result<(), Refusal> {
    if optlen.is_null() {
        return Err(Refusal::Fault);
    }
    let len = (*optlen as usize).min(size_of::<i32>());
    if len != 0 {
        if optval.is_null() {
            return Err(Refusal::Fault);
        }
        core::ptr::copy_nonoverlapping(value.to_ne_bytes().as_ptr(), optval, len);
    }
    *optlen = len as u32;
    Ok(())
}
