//! C's allocator, which Rust crates call (zlib-rs) and a C library linked
//! beside std uses: libc's source, over std's heap.

#[path = "../../../libc/src/malloc.rs"]
mod malloc;

use crate::alloc::System as HEAP;

/// errno is libc's, which std does not link: a refusal sets none
/// (`issues/the-c-allocators-set-no-errno-where-posix-has-them.md`).
fn refused(_: malloc::Refusal) {}
