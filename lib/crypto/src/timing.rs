//! Each public operation on a secret runs under the calling thread's timing
//! protection on ToyOS (`toyos::timing`): the first line of its body takes a
//! protection, which ends with the body and nests in any its caller holds.
//! Elsewhere it is nothing. Verification, hashes of public data, SipHash and
//! [`crate::ct`]'s comparisons take none; a caller holds one around SipHash
//! under a secret key. The mode does not make code constant-time; this
//! crate's code is.

#[cfg(target_os = "toyos")]
pub(crate) use toyos::timing::protect;

#[cfg(not(target_os = "toyos"))]
pub(crate) struct Protection;

#[cfg(not(target_os = "toyos"))]
#[inline(always)]
pub(crate) fn protect() -> Protection {
    Protection
}
