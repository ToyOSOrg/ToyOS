//! What a program that opens a partition through the block service links:
//! the session itself ([`session`]), and the partition as a block device over
//! it ([`disk`]); and a disk's description and a designation ([`tables`]).
//! The service is `src/main.rs` beside this.

pub mod disk;
pub mod session;
pub mod tables;

pub use session::{list, Answer, Error, Session, Unsent, Waited};
pub use toyos_blockring::client::{Outcome, Ticket};
