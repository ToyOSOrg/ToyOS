//! The memory a userland driver and its device both reach, as the boundary a
//! driver crate is written against.
//!
//! Two traits, named by what they do and not by what device is behind them:
//! [`Registers`] is one access to a mapped BAR, and [`DmaBuffers`] is the
//! grant the device reaches through its IOMMU domain, with the two barriers
//! that order the driver's accesses to it against the device's. A driver crate
//! takes each as a generic parameter, resolved at compile time, and makes
//! every decision above them, where its host tests drive it against a model of
//! its device.
//!
//! **An implementation decides nothing**: each method is one volatile load or
//! store of the width its name says, or one barrier. The program that holds
//! the claim implements both once, for every driver it runs, so what an
//! architecture needs for a store to reach a device is one body.
//!
//! **Widths are in the names**, because the width of an access to a device is
//! a decision — a register file says which each field takes — and one a
//! driver states at the site rather than has inferred. Each is there because a
//! driver in the tree makes that access.
//!
//! Every word is little-endian, as a PCI device's registers and rings are and
//! as every machine ToyOS runs on is.
//!
//! What only one driver needs of its substrate — a clock, the claim's
//! interrupt record — is that driver crate's own trait until a second driver
//! needs the same.

#![no_std]
#![forbid(unsafe_code)]

/// One access to the BAR that holds a device's registers.
///
/// **The driver bounds an offset before it reaches an implementation**:
/// against [`Self::bytes`], and aligned for the access's width. Volatile,
/// because the device writes the same bytes: a plain load of a status
/// register can be hoisted out of the loop that waits on it.
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

/// The memory this process and its device both reach: one DMA grant.
///
/// **Two addresses, because neither derives from the other**: an offset is
/// where the driver's own loads and stores land, and [`Self::device_addr`] is
/// what the device is told for the same byte. Never a physical address once a
/// unit translates for the function.
///
/// Every access is volatile and aligned for its width: the device reads and
/// writes the same bytes.
///
/// # The order of a driver's accesses against its device's
///
/// A device acts on the grant when a register tells it to, or whenever it
/// likes, and on a weakly ordered machine neither a store nor a load of this
/// memory is ordered against another without a barrier. The two are here
/// rather than beside a doorbell because the order they impose is over *this*
/// memory:
///
/// - **[`Self::publish`] before the device is pointed at what was stored.**
///   A driver stores what it publishes, calls `publish`, and only then makes
///   the store that tells the device of it — a register write, or the index
///   of a ring the device polls.
/// - **[`Self::observe`] after the load that said the device was done.** A
///   driver loads the word by which the device hands memory back, calls
///   `observe`, and only then loads what that word covers.
pub trait DmaBuffers {
    /// Bytes in the grant.
    fn bytes(&self) -> usize;
    /// Where the device reaches byte `at`.
    fn device_addr(&self, at: usize) -> u64;
    fn read16(&self, at: usize) -> u16;
    fn read32(&self, at: usize) -> u32;
    fn read64(&self, at: usize) -> u64;
    fn write16(&self, at: usize, value: u16);
    fn write32(&self, at: usize, value: u32);
    fn write64(&self, at: usize, value: u64);
    /// One release barrier: every store made above this call is visible to
    /// the device before any store made below it.
    fn publish(&self);
    /// One acquire barrier: every load made below this call sees memory at
    /// least as new as the load made above it.
    fn observe(&self);
}
