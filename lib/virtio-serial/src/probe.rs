//! Which port a function carries: the name its device gives
//! [`crate::PORT_ID`].
//!
//! **A port is named once it is ready** (§5.3.6.2): the device adds each port
//! after the driver's `DEVICE_READY`, the probe answers each `DEVICE_ADD` with
//! `PORT_READY`, and the device names the port after that. The function's
//! port is known once [`crate::PORT_ID`] is named ([`Probe::name`]).
//!
//! **What the device says about its ports is refused when it contradicts
//! itself** — a port added twice, named twice or named before it was added,
//! a name on a port the launch names none on, or a name [`control::name`]
//! refuses — and a driver ends on a refusal by its name: a device that
//! misnames its ports is not one to drive.

use alloc::string::String;

use crate::control::{self, Control, Event, NameRefused};
use crate::{MAX_PORTS, PORT_ID};

/// What the probe knows of one port id.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Port {
    Absent,
    Added,
    Named(String),
}

/// A function's ports, as its control messages describe them.
pub struct Probe {
    ports: [Port; MAX_PORTS as usize],
}

/// What the probe makes of one control buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Send the device this.
    Reply(Control),
    /// A message about the named port, the driver's to act on.
    Driver(Control),
    Nothing,
}

/// Why a control buffer is not believed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refused {
    Control(control::Refused),
    Name { id: u32, why: NameRefused },
    /// An event about a port the device never added.
    Unadded { id: u32, event: Event },
    AddedTwice { id: u32 },
    NamedTwice { id: u32 },
    /// A name on a port the launch names none on.
    Elsewhere { id: u32 },
}

impl core::fmt::Display for Refused {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Control(why) => write!(f, "{why}"),
            Self::Name { id, why } => write!(f, "port {id}'s name: {why}"),
            Self::Unadded { id, event } => write!(f, "{event:?} for port {id}, which the device never added"),
            Self::AddedTwice { id } => write!(f, "the device added port {id} twice"),
            Self::NamedTwice { id } => write!(f, "the device named port {id} twice"),
            Self::Elsewhere { id } => write!(f, "the device named port {id}, where the launch names only port {PORT_ID}"),
        }
    }
}

impl Default for Probe {
    fn default() -> Self {
        Self { ports: [const { Port::Absent }; MAX_PORTS as usize] }
    }
}

impl Probe {
    /// One control buffer the device used: its first bytes, as many as it
    /// wrote.
    pub fn take(&mut self, buffer: &[u8]) -> Result<Step, Refused> {
        let message = control::decode(buffer, MAX_PORTS).map_err(Refused::Control)?;
        let Control { id, event, .. } = message;
        let port = &mut self.ports[id as usize];
        let absent = *port == Port::Absent;
        match event {
            Event::DeviceAdd if absent => {
                *port = Port::Added;
                Ok(Step::Reply(Control { id, event: Event::PortReady, value: 1 }))
            }
            Event::DeviceAdd => Err(Refused::AddedTwice { id }),
            Event::PortName if id != PORT_ID => Err(Refused::Elsewhere { id }),
            Event::PortName if *port == Port::Added => {
                let name = control::name(buffer).map_err(|why| Refused::Name { id, why })?;
                *port = Port::Named(name.into());
                Ok(Step::Nothing)
            }
            Event::PortName if !absent => Err(Refused::NamedTwice { id }),
            _ if absent => Err(Refused::Unadded { id, event }),
            _ if id == PORT_ID => Ok(Step::Driver(message)),
            Event::DeviceRemove => {
                *port = Port::Absent;
                Ok(Step::Nothing)
            }
            _ => Ok(Step::Nothing),
        }
    }

    /// The name the device gave [`PORT_ID`], once it has.
    pub fn name(&self) -> Option<&str> {
        match &self.ports[PORT_ID as usize] {
            Port::Named(name) => Some(name),
            Port::Absent | Port::Added => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    const CONSOLE: &str = "org.toyos.console";

    fn message(id: u32, event: Event) -> Vec<u8> {
        control::encode(Control { id, event, value: 1 }).to_vec()
    }

    fn named(id: u32, name: &str) -> Vec<u8> {
        let mut buffer = message(id, Event::PortName);
        buffer.extend_from_slice(name.as_bytes());
        buffer.push(0);
        buffer
    }

    /// QEMU 11.1.1's order: the port added after `DEVICE_READY`, named after
    /// the driver's `PORT_READY`, then opened.
    #[test]
    fn the_port_is_known_once_the_device_names_it() {
        let mut probe = Probe::default();
        assert_eq!(probe.name(), None);
        assert_eq!(
            probe.take(&message(1, Event::DeviceAdd)),
            Ok(Step::Reply(Control { id: 1, event: Event::PortReady, value: 1 }))
        );
        assert_eq!(probe.name(), None);
        assert_eq!(probe.take(&named(1, CONSOLE)), Ok(Step::Nothing));
        assert_eq!(probe.name(), Some(CONSOLE));
        let open = Control { id: 1, event: Event::PortOpen, value: 1 };
        assert_eq!(probe.take(&message(1, Event::PortOpen)), Ok(Step::Driver(open)));
    }

    #[test]
    fn a_device_that_contradicts_itself_is_refused() {
        let mut probe = Probe::default();
        assert_eq!(probe.take(&named(1, CONSOLE)), Err(Refused::Unadded { id: 1, event: Event::PortName }));
        assert_eq!(probe.take(&message(1, Event::PortOpen)), Err(Refused::Unadded { id: 1, event: Event::PortOpen }));
        probe.take(&message(1, Event::DeviceAdd)).expect("the port is added");
        assert_eq!(probe.take(&message(1, Event::DeviceAdd)), Err(Refused::AddedTwice { id: 1 }));
        probe.take(&named(1, CONSOLE)).expect("the port is named");
        assert_eq!(probe.take(&named(1, CONSOLE)), Err(Refused::NamedTwice { id: 1 }));
        assert_eq!(probe.take(&message(2, Event::DeviceAdd)), Err(Refused::Control(control::Refused::Port { id: 2, max_nr_ports: 2 })));
    }

    /// A malformed name is a refusal about the port that carried it, and
    /// leaves the port unnamed.
    #[test]
    fn a_malformed_port_name_is_refused() {
        let mut probe = Probe::default();
        probe.take(&message(1, Event::DeviceAdd)).expect("the port is added");
        let mut cut = message(1, Event::PortName);
        cut.extend_from_slice(&[b'x'; control::BUFFER_BYTES]);
        cut.truncate(control::BUFFER_BYTES);
        assert_eq!(probe.take(&cut), Err(Refused::Name { id: 1, why: NameRefused::Long }));
        let mut control_byte = message(1, Event::PortName);
        control_byte.extend_from_slice(b"org.toyos.\nconsole");
        assert_eq!(probe.take(&control_byte), Err(Refused::Name { id: 1, why: NameRefused::Control { at: 10 } }));
        assert_eq!(probe.name(), None);
    }

    /// The launch names port 1 alone, so a name elsewhere is not its device.
    #[test]
    fn a_name_on_another_port_is_refused() {
        let mut probe = Probe::default();
        probe.take(&message(0, Event::DeviceAdd)).expect("port 0 is added");
        assert_eq!(probe.take(&named(0, CONSOLE)), Err(Refused::Elsewhere { id: 0 }));
    }

    /// The named port's messages are the driver's to answer; another port's
    /// removal only takes it out of the probe.
    #[test]
    fn only_the_named_ports_messages_are_the_drivers() {
        let mut probe = Probe::default();
        probe.take(&message(0, Event::DeviceAdd)).expect("port 0 is added");
        probe.take(&message(1, Event::DeviceAdd)).expect("port 1 is added");
        probe.take(&named(1, CONSOLE)).expect("port 1 is named");
        assert_eq!(probe.take(&message(0, Event::PortOpen)), Ok(Step::Nothing));
        assert_eq!(probe.take(&message(0, Event::DeviceRemove)), Ok(Step::Nothing));
        assert_eq!(probe.take(&message(0, Event::PortOpen)), Err(Refused::Unadded { id: 0, event: Event::PortOpen }));
        let removed = Control { id: 1, event: Event::DeviceRemove, value: 1 };
        assert_eq!(probe.take(&message(1, Event::DeviceRemove)), Ok(Step::Driver(removed)));
    }
}
