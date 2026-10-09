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
//! Two traits, named by what they do: [`Registers`] is one access to the BAR
//! the device's structures are in, and [`DmaBuffers`] is the memory the driver
//! and the device share. Each is a generic parameter resolved at compile time,
//! and no implementation of one decides anything: every branch is above the
//! boundary, in this crate, where the host tests it against `stub.rs`.
//!
//! # The device is not trusted
//!
//! Everything a device publishes is input from outside the driver: the
//! capabilities that say where its structures are, its feature bits, its
//! status, a queue's size and notify offset, and every word of a used ring.
//! Each is bounded before it is an offset, an index or a length, and one that
//! fails its bound is a refusal by name — [`pci::Refusal`] for the transport,
//! [`queue::UsedRefusal`] for a used ring — never a panic and never an access
//! the bound did not cover. **A refusal is the driver's to act on**: the
//! transport's ends the bring-up, and a used ring's says whether the ring can
//! still be read ([`queue::UsedRefusal::Jumped`] cannot).
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

/// One access to the BAR that holds the device's structures.
///
/// **The offsets are bounded here before any reaches an implementation**:
/// against [`Self::bytes`], and aligned for their width. Fields are
/// little-endian (§4.1.3), which is the byte order of every machine ToyOS runs
/// on, so each of these is one volatile load or store of the width its name
/// says — the width §4.1.3.1 requires of the field, chosen above this trait.
pub trait Registers {
    /// Bytes the window covers.
    fn bytes(&self) -> usize;
    fn read8(&self, at: usize) -> u8;
    fn read16(&self, at: usize) -> u16;
    fn read32(&self, at: usize) -> u32;
    fn write8(&self, at: usize, value: u8);
    fn write16(&self, at: usize, value: u16);
    fn write32(&self, at: usize, value: u32);
}

/// The memory this process and the device both reach: the grant a queue's
/// three parts are laid out in.
///
/// **Two addresses, because neither derives from the other**: an offset is
/// where this driver's loads and stores land, and [`Self::device_addr`] is
/// what the device is told for the same byte.
///
/// Ring fields are little-endian (§2.7), as [`Registers`]' are, and every
/// access is volatile: the device reads and writes the same bytes. The two
/// barriers are here because the order they impose is over this memory, so
/// what an architecture needs for a store to be visible to a device lives in
/// the implementation and nowhere above it.
pub trait DmaBuffers {
    /// Bytes in the grant.
    fn bytes(&self) -> usize;
    /// Where the device reaches byte `at`.
    fn device_addr(&self, at: usize) -> u64;
    fn read16(&self, at: usize) -> u16;
    fn read32(&self, at: usize) -> u32;
    fn write16(&self, at: usize, value: u16);
    fn write32(&self, at: usize, value: u32);
    fn write64(&self, at: usize, value: u64);
    /// One release barrier: every store above this call is visible to the
    /// device before any store below it (§2.7.13, steps 4 and 6).
    fn publish(&self);
    /// One acquire barrier: every load below this call sees memory at least as
    /// new as the load of the used index above it, so an element is never read
    /// from before the index that counts it (§2.7.8.2).
    fn observe(&self);
}
