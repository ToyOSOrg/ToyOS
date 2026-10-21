//! usbd's driver: one xHCI controller and every device on its root hub, every
//! decision it makes, and none of the instructions that carry them out.
//!
//! # The boundary
//!
//! `toyos-device-memory`'s `Registers`, one access to the mapped BAR, and
//! `DmaBuffers`, the one grant the controller reaches through its IOMMU
//! domain, and [`exec::Host`], the clock and where lines go: each a generic
//! parameter. usbd's binary is that boundary one instruction deep; on the host
//! it is the `stub` xHC, written from xHCI 1.2.
//!
//! # The shape
//!
//! [`hc`] is the register file, the rings and the operations the controller
//! holds; [`usb`] the root hub, one task per port, enumeration, and each
//! device's functions; [`hid`] and [`msc`] the two classes bound; [`input`]
//! the link to the owner of input. Everything runs as tasks on one thread
//! ([`exec`]), each waiting on one thing: the answer to the TRB it submitted,
//! a deadline, or a change another task made.
//!
//! The rest are decoders of the controller's and the devices' words, and
//! decisions with no transfer in them: [`descriptor`], [`xecp`],
//! [`protocol`], [`portsc`], [`port`], [`configure`], [`recovery`], [`bot`],
//! [`ladder`], [`identity`], [`flush`].

#![forbid(unsafe_code)]

extern crate alloc;

pub mod bot;
pub mod configure;
pub mod descriptor;
pub mod exec;
pub mod flush;
pub mod hc;
pub mod hid;
pub mod identity;
pub mod input;
pub mod invariants;
pub mod ladder;
pub mod msc;
pub mod port;
pub mod portsc;
pub mod protocol;
pub mod recovery;
pub mod usb;
pub mod xecp;

#[cfg(test)]
mod stub;
#[cfg(test)]
mod tests;

pub use portsc::{LinkState, Portsc};
pub use protocol::{Protocol, Protocols};
