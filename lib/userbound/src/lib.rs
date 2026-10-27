//! Every decision the kernel makes about the user/kernel boundary.
//!
//! **Before a dereference**: is this address userland's, and is the object at
//! it aligned for the type being read? **After a trap**: which side did the
//! frame come from? **Before an `in` or `out`**: which ports does this CPU
//! open to the process running on it, and which may no grant reach? **Before
//! an access the kernel makes for the `acpi` claim's holder**: is the address
//! the firmware's to have touched?
//!
//! [`span`] answers the first, [`fault`] the second, [`port`] the third and
//! [`firmware`] the fourth.
//!
//! Pure. No I/O, no allocation, no `unsafe`, nothing read from a device. The
//! kernel is the only caller —
//! `user_ptr.rs`, `mm/`, `syscall/`, `loader/`, `arch/x86_64/percpu.rs`,
//! `arch/x86_64/pio.rs`, `arch/x86_64/acpi_mode.rs` and `arch/x86_64/idt/exceptions.rs` — and this is a
//! crate rather than files inside it so that the boundary table below runs on
//! the host in milliseconds instead of in a boot.
//!
//! The numbers are x86-64's: [`USER_TOP`] is the canonical split at 48-bit
//! linear addresses, and a [`Ring`] comes out of a code segment selector's RPL
//! field. A second architecture brings its own; nothing else here changes.

#![no_std]
#![forbid(unsafe_code)]

pub mod fault;
pub mod idle;
pub mod firmware;
pub mod port;
pub mod span;

pub use fault::Ring;
pub use port::{port_access, IoBitmap, KeptCommands, Mediated, PortAccess, Ports, Reserved, Undeclared, IO_PORTS};
pub use span::{
    align_2m_checked, in_user_half, is_user_addr, is_user_object, rebase_base, Access, Entry,
    PAGE_2M, PAGE_4K, USER_TOP,
};
