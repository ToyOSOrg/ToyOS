//! Each public operation on a secret runs under the calling thread's timing
//! protection on ToyOS (`toyos::timing`): the first line of its body takes a
//! protection, which ends with the body. One taken inside another, as when
//! HKDF keys HMAC or a caller holds its own around a batch of operations,
//! costs a load. Elsewhere it is nothing.
//!
//! Hashes of public data, verification and SipHash take none: the first two
//! hold no secret, and a SipHash is too small an operation to switch the mode
//! for, so a caller hashing under a secret key holds the protection itself.
//! The mode does not make code constant-time; this crate's code is.

#[cfg(target_os = "toyos")]
pub(crate) use toyos::timing::protect;

#[cfg(not(target_os = "toyos"))]
pub(crate) struct Protection;

#[cfg(not(target_os = "toyos"))]
#[inline(always)]
pub(crate) fn protect() -> Protection {
    Protection
}
