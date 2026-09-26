//! The block protocol between a block service and its client.
//!
//! **A session is one shared region**, [`SESSION_BYTES`] long, that the client
//! makes and sends. Its first page holds two single-producer rings — requests
//! from the client ([`layout::SQ_BASE`]), completions from the server
//! ([`layout::CQ_BASE`]) — and the rest is the *arena*: whole
//! [`BLOCK_BYTES`] blocks a request names by index and the device moves data
//! into and out of directly. Nothing on the page is a pointer and nothing on it
//! is trusted by the end that did not write it: a consumer bounds every index
//! and every field before it acts ([`entry::Request::decode`],
//! [`ring::Consumer`]).
//!
//! **A doorbell is a byte on the session's connection**, written after the
//! entries it announces are published. The connection is also what tells each
//! end the other has gone: its hang-up is the session's end.
//!
//! **A completion means the device took the request, never that it is
//! durable.** Durability is a flush's answer, and a flush answers for its own
//! writer ([`server`], over `toyos-blockhold`): a flush whose writer's writes
//! the disk lost since they were acknowledged is answered [`Status::Lost`].
//! The client keeps every acknowledged write no flush has covered and issues
//! it again after a loss — a `Lost` flush, or a session that ended — before
//! it lets a later flush say durable ([`client::Client`]). That one mechanism
//! is what makes a server that dies survivable: what was acknowledged is on
//! the disk once a later flush says so, and what was on the wire when the
//! session ended is answered [`client::Outcome::Refused`] rather than guessed.
//!
//! **Requests in flight at once are unordered**, as on the device: a client
//! that needs one to follow another waits for the first's answer.
//!
//! Pure: `alloc` and `toyos-blockhold`, no `unsafe`. The ends that map the
//! page — `userland/blockd` and its client — hand this crate the page as
//! words ([`ring::Word`]) and act on what it answers.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod client;
pub mod entry;
pub mod layout;
pub mod ring;
pub mod server;
pub mod wire;

#[cfg(test)]
mod model;

pub use entry::{Completion, Op, Request, Status};
pub use layout::{ARENA_BLOCKS, BLOCK_BYTES, DEPTH, MAX_REQUEST_BLOCKS, SESSION_BYTES};

/// The name a block service is served under. A holder of its connector may
/// open any partition the service has; one partition's grant is a port of its
/// own ([`wire::MSG_BIND`]).
pub const PORT: &str = "block";
