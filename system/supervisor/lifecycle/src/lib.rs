//! The supervisor's lifecycle decisions, pure: the supervisor holds
//! the handles, makes the spawns and runs the loop, and asks here what to do.
//!
//! [`restart`] is when a process that ended is started again, [`claims`] which
//! claims a start mints, [`drives`] which PCI functions a `drives` row has
//! a process on, [`storage`] which partitions a file server's role reaches,
//! [`sessions`] when a login session may be opened and signed out,
//! [`stop`] how the machine's stop goes, [`boot`] which controller
//! `@boot` names, and [`declaration`] when storage and the PCI drivers start.

pub mod boot;
pub mod claims;
pub mod declaration;
pub mod drives;
pub mod restart;
pub mod storage;
pub mod sessions;
pub mod stop;
