//! The calling thread's timing protection on ToyOS (`toyos::timing`); off
//! ToyOS, [`protect`] is nothing.
//!
//! The boundary is the operation that is secret by construction: every
//! operation that computes on a key or other secret takes a protection on its
//! first line, which ends with it and nests in any its caller holds. Ciphers,
//! MACs, KDFs, keyed SipHash, [`crate::ct::equal`] and every secret-key and
//! agreement operation do. A plain hash serves public data as often as secret
//! and takes none, and verification holds no secret: **a caller hashing a
//! secret takes [`protect`] around it itself.**

#[cfg(target_os = "toyos")]
pub use toyos::timing::{protect, Protection};

#[cfg(not(target_os = "toyos"))]
#[must_use = "the protection ends when this drops"]
pub struct Protection;

#[cfg(not(target_os = "toyos"))]
impl Protection {
    /// Never: there is no mode off ToyOS.
    pub fn is_active(&self) -> bool {
        false
    }
}

#[cfg(not(target_os = "toyos"))]
#[inline(always)]
pub fn protect() -> Protection {
    Protection
}
