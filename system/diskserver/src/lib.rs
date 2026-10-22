//! What a program that opens a partition through the block service links:
//! the session itself ([`session`]), and the partition as a block device over
//! it ([`disk`]). The service is `src/main.rs` beside this.

pub mod disk;
pub mod session;

pub use session::{list, Answer, Error, Session, Unsent, Waited};
pub use toyos_blockring::client::{Outcome, Ticket};
