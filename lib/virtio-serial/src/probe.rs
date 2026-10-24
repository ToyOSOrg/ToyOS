//! Which function is a driver's: the one whose device names a port with the
//! driver's name.
//!
//! **A port is named once it is ready** (§5.3.6.2): the device adds each port
//! after the driver's `DEVICE_READY`, the probe answers each `DEVICE_ADD` with
//! `PORT_READY`, and the device names the port after that. The function is
//! the driver's once a port carries its name, at [`crate::PORT_ID`], and
//! another driver's once every port it added is named and none is.
//!
//! **What the device says about its ports is refused when it contradicts
//! itself** — a port added twice, named twice or named before it was added,
//! or a name [`control::name`] refuses — and a driver ends on a refusal by its
//! name: a device that misnames its ports is not one to drive.

use alloc::string::String;
use alloc::vec::Vec;

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
pub struct Probe<'w> {
    wanted: &'w str,
    ports: [Port; MAX_PORTS as usize],
}

/// What the probe makes of one control buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Send the device this.
    Reply(Control),
    /// A message about a port the probe knows, the driver's to act on.
    Driver(Control),
    Nothing,
}

/// Whose the function is, so far.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Probing,
    /// A port at [`PORT_ID`] carries the driver's name.
    Ours,
    /// Every port the device added is named, by these names, and none is the
    /// driver's.
    Foreign(Vec<String>),
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
    /// The driver's name on a port the launch does not put it on.
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
            Self::Elsewhere { id } => write!(f, "the device gave port {id} the name the launch puts on port {PORT_ID}"),
        }
    }
}

impl<'w> Probe<'w> {
    pub fn new(wanted: &'w str) -> Self {
        Self { wanted, ports: [const { Port::Absent }; MAX_PORTS as usize] }
    }

    /// One control buffer the device used: its first bytes, as many as it
    /// wrote.
    pub fn take(&mut self, buffer: &[u8]) -> Result<Step, Refused> {
        let message = control::decode(buffer, MAX_PORTS).map_err(Refused::Control)?;
        let Control { id, event, .. } = message;
        let wanted = self.wanted;
        let port = &mut self.ports[id as usize];
        let absent = *port == Port::Absent;
        match event {
            Event::DeviceAdd if absent => {
                *port = Port::Added;
                Ok(Step::Reply(Control { id, event: Event::PortReady, value: 1 }))
            }
            Event::DeviceAdd => Err(Refused::AddedTwice { id }),
            Event::PortName if *port == Port::Added => {
                let name = control::name(buffer).map_err(|why| Refused::Name { id, why })?;
                if name == wanted && id != PORT_ID {
                    return Err(Refused::Elsewhere { id });
                }
                *port = Port::Named(name.into());
                Ok(Step::Nothing)
            }
            Event::PortName if !absent => Err(Refused::NamedTwice { id }),
            _ if absent => Err(Refused::Unadded { id, event }),
            Event::DeviceRemove if *port == Port::Named(wanted.into()) => Ok(Step::Driver(message)),
            Event::DeviceRemove => {
                *port = Port::Absent;
                Ok(Step::Nothing)
            }
            _ => Ok(Step::Driver(message)),
        }
    }

    pub fn verdict(&self) -> Verdict {
        let named = || self.ports.iter().filter_map(|port| match port {
            Port::Named(name) => Some(name),
            Port::Absent | Port::Added => None,
        });
        if named().any(|name| name == self.wanted) {
            return Verdict::Ours;
        }
        let unnamed = self.ports.contains(&Port::Added);
        match named().next() {
            Some(_) if !unnamed => Verdict::Foreign(named().cloned().collect()),
            _ => Verdict::Probing,
        }
    }

    /// Whether the driver's port is the one `id` names.
    pub fn is_ours(&self, id: u32) -> bool {
        matches!(self.ports.get(id as usize), Some(Port::Named(name)) if name == self.wanted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec;

    const CONSOLE: &str = "org.toyos.console";
    const AGENT: &str = "com.redhat.spice.0";

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
    fn a_port_with_the_drivers_name_makes_the_function_its_own() {
        let mut probe = Probe::new(CONSOLE);
        assert_eq!(probe.verdict(), Verdict::Probing);
        assert_eq!(
            probe.take(&message(1, Event::DeviceAdd)),
            Ok(Step::Reply(Control { id: 1, event: Event::PortReady, value: 1 }))
        );
        assert_eq!(probe.verdict(), Verdict::Probing);
        assert_eq!(probe.take(&named(1, CONSOLE)), Ok(Step::Nothing));
        assert_eq!(probe.verdict(), Verdict::Ours);
        assert!(probe.is_ours(1) && !probe.is_ours(0));
        let open = Control { id: 1, event: Event::PortOpen, value: 1 };
        assert_eq!(probe.take(&message(1, Event::PortOpen)), Ok(Step::Driver(open)));
    }

    /// The clipboard's function, probed by the console's driver: another
    /// driver's once its one port is named.
    #[test]
    fn a_function_whose_ports_are_all_named_otherwise_is_anothers() {
        let mut probe = Probe::new(CONSOLE);
        probe.take(&message(1, Event::DeviceAdd)).expect("the port is added");
        probe.take(&named(1, AGENT)).expect("the port is named");
        assert_eq!(probe.verdict(), Verdict::Foreign(vec![AGENT.to_string()]));
    }

    /// A port still unnamed holds the verdict, whatever the others are.
    #[test]
    fn an_unnamed_port_leaves_the_function_undecided() {
        let mut probe = Probe::new(AGENT);
        probe.take(&message(0, Event::DeviceAdd)).expect("port 0 is added");
        probe.take(&message(1, Event::DeviceAdd)).expect("port 1 is added");
        probe.take(&named(0, "some.other.port")).expect("port 0 is named");
        assert_eq!(probe.verdict(), Verdict::Probing);
        probe.take(&named(1, AGENT)).expect("port 1 is named");
        assert_eq!(probe.verdict(), Verdict::Ours);
    }

    #[test]
    fn a_device_that_contradicts_itself_is_refused() {
        let mut probe = Probe::new(CONSOLE);
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
        let mut probe = Probe::new(CONSOLE);
        probe.take(&message(1, Event::DeviceAdd)).expect("the port is added");
        let mut cut = message(1, Event::PortName);
        cut.extend_from_slice(&[b'x'; control::BUFFER_BYTES]);
        cut.truncate(control::BUFFER_BYTES);
        assert_eq!(probe.take(&cut), Err(Refused::Name { id: 1, why: NameRefused::Long }));
        let mut control_byte = message(1, Event::PortName);
        control_byte.extend_from_slice(b"org.toyos.\nconsole");
        assert_eq!(probe.take(&control_byte), Err(Refused::Name { id: 1, why: NameRefused::Control { at: 10 } }));
        assert_eq!(probe.verdict(), Verdict::Probing);
    }

    /// The driver's name is where the launch puts it, or the device is not
    /// the one the launch made.
    #[test]
    fn the_drivers_name_on_another_port_is_refused() {
        let mut probe = Probe::new(CONSOLE);
        probe.take(&message(0, Event::DeviceAdd)).expect("port 0 is added");
        assert_eq!(probe.take(&named(0, CONSOLE)), Err(Refused::Elsewhere { id: 0 }));
    }

    /// The removal of the driver's port is the driver's to answer; another
    /// port's removal only takes it out of the verdict.
    #[test]
    fn a_removal_is_the_drivers_only_for_its_own_port() {
        let mut probe = Probe::new(CONSOLE);
        probe.take(&message(0, Event::DeviceAdd)).expect("port 0 is added");
        probe.take(&message(1, Event::DeviceAdd)).expect("port 1 is added");
        probe.take(&named(1, CONSOLE)).expect("port 1 is named");
        assert_eq!(probe.take(&message(0, Event::DeviceRemove)), Ok(Step::Nothing));
        let removed = Control { id: 1, event: Event::DeviceRemove, value: 1 };
        assert_eq!(probe.take(&message(1, Event::DeviceRemove)), Ok(Step::Driver(removed)));
    }
}
