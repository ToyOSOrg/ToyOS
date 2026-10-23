//! The supervisor's lifecycle decisions, pure: the supervisor holds
//! the handles, makes the spawns and runs the loop, and asks here what to do.
//!
//! [`restart`] is when a process that ended is started again, [`claims`] which
//! claims a start mints, [`drives`] which PCI functions a `drives` row has
//! a process on, and [`storage`] which partitions a file server's role reaches.

pub mod claims;
pub mod drives;
pub mod restart;
pub mod storage;
