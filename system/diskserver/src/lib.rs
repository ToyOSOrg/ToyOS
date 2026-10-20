//! What a program that opens a partition through the block service links:
//! the session itself ([`session`]), and a partition's blocks however it is
//! held ([`disk`]). The service is `src/main.rs` beside this.

pub mod disk;
pub mod session;

pub use session::{list, Answer, Error, Session, Unsent, Waited};
pub use toyos_blockring::client::{Outcome, Ticket};
