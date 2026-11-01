//! QEMU's clipboard channel, as a guest agent speaks it: the agent's port's
//! name and the spice vdagent stream ([`stream`] decodes it, [`encode`] writes
//! it), and the port between the virtio-serial driver and the agent
//! ([`endpoint`]).
//!
//! **Every word decoded here is the host's**, written by QEMU's `qemu-vdagent`
//! chardev through its virtio-serial device. Each is bounded before it sizes a
//! buffer, and one that fails its bound is a refusal by name: a decode never
//! panics and never buffers more than arrived.
//!
//! What it cites: spice-protocol's `spice/vd_agent.h` at `499cc832` for the stream, its
//! packed layouts and its numbers. The header leaves a `GRAB`'s serial without
//! a meaning; what QEMU 11.1.1 does with it is the agent's to state, measured.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod encode;
pub mod endpoint;
pub mod stream;

/// The agent's port, as `vd_agent.h:63-68` names it: what the launch names the
/// `virtserialport` it creates, and how the driver knows the port is the
/// agent's.
pub const PORT_NAME: &str = "com.redhat.spice.0";

/// The longest clipboard text that crosses, in bytes: the compositor's
/// `window::MAX_CLIPBOARD_BYTES`, which the agent asserts this equals.
pub const MAX_TEXT: usize = 2 * 1024 * 1024;

/// The largest transmit the agent makes: its capabilities, then a push of
/// [`MAX_TEXT`] — `GRAB [serial][UTF8_TEXT]` and its `CLIPBOARD` — each message
/// chunked at [`stream::MAX_CHUNK`].
pub const LARGEST_TRANSMIT: usize = encode::message_bytes(encode::CAPABILITIES_DATA)
    + encode::message_bytes(encode::GRAB_DATA)
    + encode::message_bytes(encode::clipboard_data(MAX_TEXT));

/// `vd_agent.h`'s numbers this channel uses.
pub mod wire {
    /// `VDP_CLIENT_PORT`, the only `VDIChunkHeader.port` the agent reads.
    pub const CLIENT_PORT: u32 = 1;
    /// `VD_AGENT_PROTOCOL`.
    pub const PROTOCOL: u32 = 1;
    /// `VDIChunkHeader`: `port` and `size`.
    pub const CHUNK_HEADER: usize = 8;
    /// `VDAgentMessage` up to `data`: `protocol`, `type`, `opaque`, `size`.
    pub const MESSAGE_HEADER: usize = 20;

    /// The message types.
    pub const CLIPBOARD: u32 = 4;
    pub const ANNOUNCE_CAPABILITIES: u32 = 6;
    pub const GRAB: u32 = 7;
    pub const REQUEST: u32 = 8;
    pub const RELEASE: u32 = 9;

    /// `VD_AGENT_CLIPBOARD_UTF8_TEXT`.
    pub const UTF8_TEXT: u32 = 1;

    /// The capability bits, as `VD_AGENT_SET_CAPABILITY` sets them in the
    /// first word.
    pub const CAP_CLIPBOARD_BY_DEMAND: u32 = 1 << 5;
    pub const CAP_CLIPBOARD_GRAB_SERIAL: u32 = 1 << 17;
}
