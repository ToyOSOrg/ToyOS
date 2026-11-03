//! Cryptographic primitives for TLS, SSH, boot verification, passphrase-derived
//! keys and WPA2.
//!
//! The core has no allocator, entropy source, host build tool or runtime
//! dependency but, on ToyOS, its timing protection (`timing`). Inputs
//! crossing an encoding boundary return [`Error`]; fixed-size
//! representations hold the invariants inside that boundary. Message lengths,
//! algorithm choices and CPU capabilities are public. Module contracts identify
//! operations permitted to depend on public data.
//!
//! Secret owners and secret operations' workspaces are erased as `secret`'s
//! contract states; arithmetic intermediates, registers and compiler-made
//! copies are not. Constant-time source structure requires validation of the
//! compiler and hardware used to deploy it.

#![no_std]
#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(any(test, feature = "std"))]
extern crate std;

#[cfg(feature = "rustls-provider")]
extern crate alloc;

pub mod aead;
mod aes;
mod arch;
mod block;
pub mod chacha;
pub mod ct;
pub mod ecc;
pub mod ed25519;
mod field25519;
mod gcm;
pub mod hash;
pub mod hybrid;
pub mod kdf;
mod keccak;
pub mod mlkem;
mod montgomery;
mod naf;
mod nist;
pub mod poly1305;
pub mod rsa;
#[cfg(feature = "rustls-provider")]
pub mod rustls_provider;
mod scalar25519;
pub mod scrypt;
mod secret;
pub use secret::{Secret, Zeroable};
pub mod siphash;
pub mod ssh;
pub mod timing;
#[cfg(feature = "wpa2")]
pub mod wpa2;
pub mod x25519;

#[cfg(test)]
mod test_arithmetic;

/// Refusal at a cryptographic input boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Length,
    Authentication,
    InvalidKey,
    InvalidPoint,
    InvalidEncoding,
    InvalidSignature,
    CounterExhausted,
    Entropy,
    InvalidParameters,
}

/// Fills an entire destination from the caller's cryptographically secure source.
/// A failure must not be treated as a successful partial fill.
pub trait Random {
    fn fill(&mut self, destination: &mut [u8]) -> Result<(), Error>;
}
