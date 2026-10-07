//! ToyOS-specific extensions to primitives in the [`std::ffi`] module
//!
//! [`std::ffi`]: crate::ffi

#[path = "../../../rust/library/std/src/os/unix/ffi/os_str.rs"]
mod os_str;

#[stable(feature = "toyos_ext", since = "1.0.0")]
pub use self::os_str::{OsStrExt, OsStringExt};
