//! Every decision the kernel makes about the user/kernel boundary.
//!
//! **Before a dereference**: is this
//! address userland's, is the object at it aligned for the type being read, and
//! does it lie wholly inside one mapping? **Before a placement**: can a length
//! userland asked for be placed at all, and where does it go? **After a trap**:
//! which side did the frame come from?
//!
//! [`span`] answers the first, [`place`] the second and [`fault`] the third.
//!
//! Pure. No I/O, no allocation, no `unsafe`, nothing read from a device and
//! nothing named outside this crate. The kernel is the only caller —
//! `user_ptr.rs`, `mm/`, `syscall/`, `loader/` and
//! `arch/x86_64/idt/exceptions.rs` — and this is a crate rather than files inside it so
//! that the boundary table below runs on the host in milliseconds instead of in
//! a boot.
//!
//! The numbers are x86-64's: [`USER_TOP`] is the canonical split at 48-bit
//! linear addresses, [`PAGE_2M`] is the kernel's one user page size, and a
//! [`Ring`] comes out of a code segment selector's RPL field. A second
//! architecture brings its own three; nothing else here changes.

#![no_std]
#![forbid(unsafe_code)]

pub mod fault;
pub mod place;
pub mod span;

pub use fault::Ring;
pub use place::{PageSpan, Window};
pub use span::{
    align_2m_checked, contiguous, in_user_half, is_user_addr, is_user_object, rebase_base, Access,
    PAGE_2M, PAGE_4K, USER_TOP,
};
