//! The link protocol between the network stack and each NIC driver.
//!
//! **A link is one connection and one region.** A driver connects to the
//! stack's [`PORT`] and sends a region it made with its hello
//! ([`wire`]). The region's header page holds two `toyos::transport` rings of
//! frame lengths, received frames from the driver and frames to send from the
//! stack, and the words each side states and asks with ([`layout`],
//! [`state`]); its arena holds a slot per ring entry, which a frame is copied
//! into and out of once on each side ([`ends`]).
//!
//! **A doorbell is an event**, rung after what it announces is in the
//! region: the stack makes both and sends the driver its ends with its
//! answer ([`wire`]), so it holds no event a driver sent. A bell whose waker
//! is gone is the link's
//! end: the stack keeps every connection it carries across it, and the
//! driver started again opens a new link.
//!
//! The rings, the frames' lengths and every state word are pure and
//! host-tested; [`region`] maps the region and [`driver`] carries a device's
//! frames over it.

#![cfg_attr(not(test), no_std)]
#![deny(unsafe_code)]

extern crate alloc;

pub mod driver;
pub mod ends;
pub mod layout;
pub mod region;
pub mod state;
pub mod wire;

/// The name the stack is served to NIC drivers under.
pub const PORT: &str = "link";
