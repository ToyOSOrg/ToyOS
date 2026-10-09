//! The virtio PCI transport and the split virtqueue — every decision a driver
//! makes about them, and none of the instructions that carry them out.
//!
//! Every `§` in this crate is a section of *Virtual I/O Device (VIRTIO)
//! Version 1.2*, OASIS Committee Specification 01. [`pci`] is §4.1 with the
//! initialisation §3.1.1 orders and the negotiation §2.2 bounds; [`queue`] is
//! §2.7. A device type — its feature bits, its configuration fields, what its
//! buffers carry — is its driver's and is not here.
//!
//! # The boundary
//!
//! `toyos-device-memory`'s two traits, the ones every userland driver is
//! written against: `Registers` is one access to the BAR the device's
//! structures are in, and `DmaBuffers` is the grant a queue's three parts are
//! laid out in. Each is a generic parameter resolved at compile time, and no
//! implementation of one decides anything: every branch is above the
//! boundary, in this crate, where the host tests it against `stub.rs`.
//!
//! **The offsets are bounded here before any reaches an implementation**, and
//! **the width of each register access is §4.1.3.1's**, chosen here. The two
//! barriers are where §2.7.13 and §2.7.8.2 put them ([`queue`]).
//!
//! # The device is not trusted
//!
//! Everything a device publishes is input from outside the driver: the
//! capabilities that say where its structures are, its feature bits, its
//! status, a queue's size and notify offset, and every word of a used ring.
//! Each is bounded before it is an offset, an index or a length, and one that
//! fails its bound is a refusal by name — [`pci::Refusal`] for the transport,
//! [`queue::UsedRefusal`] for a used ring — never a panic and never an access
//! the bound did not cover.
//!
//! **A refusal is the end of the device's use.** The transport's ends the
//! bring-up, and takes the [`pci::Setup`] it was made on with it. A used
//! ring's is something no conforming device writes (§2.7.8, §2.7.8.2), and
//! the queue it was read from is not one to go on reading: the element was
//! spent, and the used index may never pass the available one, so every chain
//! still in flight is one element short of coming back.
//!
//! What panics here is the *driver's* own mistake — a chain published over
//! descriptors still in flight, a ring laid out past its grant — which no
//! device can cause.
//!
//! # What is not here
//!
//! - **No notification suppression, either way.** No `VIRTIO_F_EVENT_IDX` is
//!   accepted and `avail.flags` stays 0, so §2.7.7.2 has the device notify for
//!   every buffer it uses; and every published chain is notified, which
//!   §2.7.10.1 permits whatever `used.flags` says — reading it would buy one
//!   skipped register write for one more device-written word believed.
//! - **No packed ring, no indirect descriptors, no legacy interface.**
//! - **No walk of configuration space**: [`pci::Layout::of`] takes the vendor
//!   capabilities a driver read through its claim.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod pci;
pub mod queue;

#[cfg(test)]
mod stub;
#[cfg(test)]
mod tests;
