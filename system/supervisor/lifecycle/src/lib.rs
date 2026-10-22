//! The supervisor's lifecycle decisions, pure: the supervisor holds
//! the handles, makes the spawns and runs the loop, and asks here what to do.
//!
//! [`restart`] is when a process that ended is started again, [`claims`] which
//! claims a start mints, and [`drives`] which PCI functions a `drives` row has
//! a process on.

pub mod claims;
pub mod drives;
pub mod restart;
