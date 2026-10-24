//! virtio-serial as this project's two drivers of it speak it: the shared
//! clipboard's agent (`/system/bin/vdagent`) and the VM's console
//! (`/system/bin/virtio-console`).
//!
//! **Both are the same device, `1af4:1043`, and only a port's name says which
//! a function is**: [`probe`] reads the names the device gives its ports, and
//! a driver that finds no port of its own lets the function go to the next
//! row that drives one. [`control`] is the control messages and the names,
//! decoded bounded; `toyos-virtio-serial-claim` brings the function up over
//! its claim.
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

/// The VM console's port, as the launch names it and its driver probes for.
pub const CONSOLE_PORT: &str = "org.toyos.console";

/// Every virtio-serial function the launch makes has `max_ports` this: port 0,
/// which nothing here uses, and the function's one named port.
pub const MAX_PORTS: u32 = 2;

/// The id of a function's one named port, `nr=` at the launch.
pub const PORT_ID: u32 = 1;

/// How many control receive buffers a driver keeps posted: virtio-serial
/// drops a control event that finds none.
pub const CONTROL_BUFFERS: usize = 8;
