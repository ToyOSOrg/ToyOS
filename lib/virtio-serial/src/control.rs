//! virtio-serial's control messages (virtio 1.2 §5.3.6.2): `struct
//! virtio_console_control { le32 id; le16 event; le16 value; }`, each in a
//! buffer of its own on the control queues, and a `PORT_NAME`'s name behind
//! its message.
//!
//! **A name is UTF-8 with no control character, at most [`MAX_NAME`] bytes.**
//! §5.3.6.2 has it follow the message "without a NUL terminator", and QEMU
//! 11.1.1 ends it with one, which is not part of it.

/// One control message's bytes.
pub const BYTES: usize = 8;

/// The longest port name read.
pub const MAX_NAME: usize = 64;

/// A control receive buffer: a message and the longest name, its NUL, and a
/// byte more, so a name the buffer cut is one that fills it.
pub const BUFFER_BYTES: usize = BYTES + MAX_NAME + 2;

/// §5.3.6.2's events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    DeviceReady,
    DeviceAdd,
    DeviceRemove,
    PortReady,
    ConsolePort,
    Resize,
    PortOpen,
    PortName,
}

impl Event {
    const ALL: [Self; 8] = [
        Self::DeviceReady,
        Self::DeviceAdd,
        Self::DeviceRemove,
        Self::PortReady,
        Self::ConsolePort,
        Self::Resize,
        Self::PortOpen,
        Self::PortName,
    ];

    /// The event's number, its index in §5.3.6.2's list.
    fn number(self) -> u16 {
        Self::ALL.iter().position(|e| *e == self).expect("every event is in ALL") as u16
    }
}

/// One control message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Control {
    pub id: u32,
    pub event: Event,
    pub value: u16,
}

/// Why a control buffer the device used is not a control message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// Fewer bytes than a message.
    Short { len: usize },
    /// An event §5.3.6.2 does not list.
    Event { event: u16 },
    /// A port id at or past the device's `max_nr_ports`.
    Port { id: u32, max_nr_ports: u32 },
}

impl core::fmt::Display for Refused {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Short { len } => write!(f, "a control buffer of {len} byte(s), short of a message's {BYTES}"),
            Self::Event { event } => write!(f, "a control message of event {event}, which virtio-serial does not define"),
            Self::Port { id, max_nr_ports } => {
                write!(f, "a control message for port {id}, of a device with {max_nr_ports} port(s)")
            }
        }
    }
}

/// The control message at the start of `bytes`, a buffer the device wrote,
/// for a device of `max_nr_ports` ports. What follows the message — a
/// `PORT_NAME`'s name — is [`name`]'s.
pub fn decode(bytes: &[u8], max_nr_ports: u32) -> Result<Control, Refused> {
    let Some(message) = bytes.get(..BYTES) else {
        return Err(Refused::Short { len: bytes.len() });
    };
    let id = u32::from_le_bytes([message[0], message[1], message[2], message[3]]);
    let event = u16::from_le_bytes([message[4], message[5]]);
    let value = u16::from_le_bytes([message[6], message[7]]);
    let event = *Event::ALL.get(usize::from(event)).ok_or(Refused::Event { event })?;
    if id >= max_nr_ports {
        return Err(Refused::Port { id, max_nr_ports });
    }
    Ok(Control { id, event, value })
}

/// A control message's bytes.
pub fn encode(control: Control) -> [u8; BYTES] {
    let mut bytes = [0; BYTES];
    bytes[..4].copy_from_slice(&control.id.to_le_bytes());
    bytes[4..6].copy_from_slice(&control.event.number().to_le_bytes());
    bytes[6..].copy_from_slice(&control.value.to_le_bytes());
    bytes
}

/// Why the bytes behind a `PORT_NAME` are not a name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameRefused {
    Empty,
    /// Past [`MAX_NAME`] bytes, or cut where the buffer ended.
    Long,
    /// Not UTF-8.
    Encoding,
    /// A control character, a NUL inside it among them, at this byte.
    Control { at: usize },
}

impl core::fmt::Display for NameRefused {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Empty => write!(f, "an empty port name"),
            Self::Long => write!(f, "a port name past {MAX_NAME} bytes"),
            Self::Encoding => write!(f, "a port name that is not UTF-8"),
            Self::Control { at } => write!(f, "a port name with a control character at byte {at}"),
        }
    }
}

/// The name a `PORT_NAME` message in `buffer` carries: `buffer` is the
/// [`BUFFER_BYTES`] buffer's first bytes, as many as the device wrote.
pub fn name(buffer: &[u8]) -> Result<&str, NameRefused> {
    let after = buffer.get(BYTES..).unwrap_or_default();
    let name = after.strip_suffix(&[0]).unwrap_or(after);
    if name.is_empty() {
        return Err(NameRefused::Empty);
    }
    if name.len() > MAX_NAME || buffer.len() >= BUFFER_BYTES {
        return Err(NameRefused::Long);
    }
    let name = core::str::from_utf8(name).map_err(|_| NameRefused::Encoding)?;
    match name.char_indices().find(|(_, c)| c.is_control()) {
        Some((at, _)) => Err(NameRefused::Control { at }),
        None => Ok(name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    /// §5.3.6.2's layout and numbering: `DEVICE_READY` is 0, `PORT_READY` 3,
    /// `PORT_OPEN` 6, little-endian.
    #[test]
    fn each_control_message_is_its_layout() {
        assert_eq!(encode(Control { id: 0, event: Event::DeviceReady, value: 1 }), [0, 0, 0, 0, 0, 0, 1, 0]);
        assert_eq!(encode(Control { id: 1, event: Event::PortReady, value: 1 }), [1, 0, 0, 0, 3, 0, 1, 0]);
        assert_eq!(encode(Control { id: 1, event: Event::PortOpen, value: 0 }), [1, 0, 0, 0, 6, 0, 0, 0]);
        let numbers: Vec<u16> = Event::ALL.iter().map(|e| e.number()).collect();
        assert_eq!(numbers, [0, 1, 2, 3, 4, 5, 6, 7]);
    }

    fn port_name(name: &[u8]) -> Vec<u8> {
        let mut buffer = Vec::from([1, 0, 0, 0, 7, 0, 1, 0]);
        buffer.extend_from_slice(name);
        buffer
    }

    /// A `PORT_NAME` with its name behind it decodes as the message alone,
    /// and its name with or without QEMU's NUL.
    #[test]
    fn a_port_name_is_its_message_and_its_name() {
        let buffer = port_name(b"com.redhat.spice.0\0");
        assert_eq!(decode(&buffer, 2), Ok(Control { id: 1, event: Event::PortName, value: 1 }));
        assert_eq!(name(&buffer), Ok("com.redhat.spice.0"));
        assert_eq!(name(&port_name(b"org.toyos.console")), Ok("org.toyos.console"));
    }

    #[test]
    fn a_malformed_port_name_is_refused_by_what_is_wrong() {
        assert_eq!(name(&port_name(b"")), Err(NameRefused::Empty));
        assert_eq!(name(&port_name(b"\0")), Err(NameRefused::Empty));
        assert_eq!(name(&[1, 0, 0, 0, 7, 0, 1]), Err(NameRefused::Empty));
        assert_eq!(name(&port_name(b"a\0b\0")), Err(NameRefused::Control { at: 1 }));
        assert_eq!(name(&port_name(b"tab\there")), Err(NameRefused::Control { at: 3 }));
        assert_eq!(name(&port_name(b"\xff\xfe")), Err(NameRefused::Encoding));
        assert_eq!(name(&port_name(&[b'a'; MAX_NAME])), Ok(core::str::from_utf8(&[b'a'; MAX_NAME]).unwrap()));
        assert_eq!(name(&port_name(&[b'a'; MAX_NAME + 1])), Err(NameRefused::Long));
    }

    /// A name the receive buffer cut fills the whole buffer, so a buffer the
    /// device filled is never read as a shorter name.
    #[test]
    fn a_name_the_buffer_cut_is_refused() {
        let cut = &port_name(&[b'a'; 200])[..BUFFER_BYTES];
        assert_eq!(name(cut), Err(NameRefused::Long));
    }

    #[test]
    fn a_port_id_at_max_nr_ports_is_refused() {
        assert_eq!(decode(&[2, 0, 0, 0, 1, 0, 1, 0], 2), Err(Refused::Port { id: 2, max_nr_ports: 2 }));
        assert_eq!(decode(&[1, 0, 0, 0, 1, 0, 1, 0], 2), Ok(Control { id: 1, event: Event::DeviceAdd, value: 1 }));
    }

    #[test]
    fn an_undefined_event_is_refused() {
        assert_eq!(decode(&[1, 0, 0, 0, 8, 0, 1, 0], 2), Err(Refused::Event { event: 8 }));
    }

    #[test]
    fn a_short_buffer_is_refused() {
        assert_eq!(decode(&[1, 0, 0, 0, 6, 0, 1], 2), Err(Refused::Short { len: 7 }));
    }
}
