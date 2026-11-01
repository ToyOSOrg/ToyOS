//! virtio-serial, the virtio console device (virtio 1.2 §5.3), as
//! `/system/bin/virtio-console` drives it: one process per function, handing
//! the function's one named port to the service the name is for.
//!
//! [`control`] is the control messages and the names, decoded bounded;
//! [`probe`] reads the name the device gives the function's port.
//!
//! What it cites: *Virtual I/O Device (VIRTIO) Version 1.2*, OASIS Committee
//! Specification 01, §5.3 for the console device and its control messages.
//!
//! Pure: `core` and `alloc`, no `unsafe`, no I/O.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod control;
pub mod probe;

/// The VM console's port, as the launch names it.
pub const CONSOLE_PORT: &str = "org.toyos.console";

/// Every virtio-serial function the launch makes has `max_ports` this: port 0,
/// which nothing here uses, and the function's one named port.
pub const MAX_PORTS: u32 = 2;

/// The id of a function's one named port, `nr=` at the launch.
pub const PORT_ID: u32 = 1;

/// How many control receive buffers a driver keeps posted: virtio-serial
/// drops a control event that finds none.
pub const CONTROL_BUFFERS: usize = 8;
