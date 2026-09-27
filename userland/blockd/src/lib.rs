//! What a program that opens a partition through a block service links — the
//! session region as a process sees it ([`region`]) and the session itself
//! ([`session`]) — and the NVMe driver the service runs ([`nvme`]), which is a
//! library so a test can aim the controller at an address itself. The service
//! is `src/main.rs` beside this.

pub mod nvme;
pub mod region;
pub mod session;

pub use session::{Answer, Error, Session, Unsent, Waited};
pub use toyos_blockring::client::{Outcome, Ticket};
