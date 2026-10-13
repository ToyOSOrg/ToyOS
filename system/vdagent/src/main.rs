//! The shared clipboard with QEMU's host: the guest agent of QEMU's
//! `qemu-vdagent` chardev, between the virtio-serial function it is on and
//! the compositor's clipboard.
//!
//! **What is here is the loop and the device's handshake.** The function is
//! [`channel`]'s; the stream is `toyos-vdagent`'s; every decision about who
//! owns the clipboard and what a transmit owes is `toyos-vdagent-bridge`'s;
//! the compositor's side is `window::watcher`'s.
//!
//! **The handshake is virtio 1.2 §5.3.6.2's**: `DEVICE_READY`, the device's
//! `DEVICE_ADD` of [`PORT_ID`] answered with `PORT_READY`, and the agent's
//! port opened once the host's `PORT_OPEN(1)` has come. The host's
//! `PORT_OPEN(0)` is answered with the agent's `PORT_OPEN(0)`, and its
//! `PORT_OPEN(1)` with `PORT_OPEN(1)`: QEMU 11.1.1 then starts a new session
//! with its capabilities, measured. Any other port the device adds, a console
//! port, the removal of the agent's port, an unframed stream and a used ring
//! the device is not believed on end the program by name. It is not started
//! again: one that ended says why and stays down for the boot.
//!
//! **A transmit the device has not taken back within [`TRANSMIT_BOUND`] ends
//! the program**: QEMU 11.1.1 takes the largest one at once, measured, and a
//! device holding one holds every message after it.
//!
//! **No clipboard byte is ever logged**: lengths, serials and refusals only.

use std::time::{Duration, Instant};

use toyos::endow;
use toyos::poller::{Poller, READABLE};
use toyos::say;
use toyos::PciDev;
use toyos_abi::syscall::PciId;
use toyos_vdagent::control::{Control, Event};
use toyos_vdagent::stream::{Decoder, Message};
use toyos_vdagent::PORT_ID;
use toyos_vdagent_bridge::sets::Sets;
use toyos_vdagent_bridge::{Bridge, Effect};
use window::watcher::{Watched, Watcher};

mod channel;

use channel::Channel;

/// One declaration of the clipboard's bound, read by the codec and the
/// compositor alike.
const _: () = assert!(toyos_vdagent::MAX_TEXT == window::MAX_CLIPBOARD_BYTES);

/// Virtio 1.2 §5.3.1: the console device is `0x1040 + 3`.
const VIRTIO_SERIAL: PciId = PciId { vendor: 0x1af4, device: 0x1043 };

/// How long a transmit may stay with the device. Policy, far past any time
/// QEMU 11.1.1 took, measured: it took every element at once.
const TRANSMIT_BOUND: Duration = Duration::from_secs(5);

const TOKEN_CLAIM: u64 = 0;
const TOKEN_WATCHER: u64 = 1;

/// The agent's state beside the bridge's: the device's handshake and the
/// stream's decode.
struct Agent {
    bridge: Bridge,
    decoder: Decoder,
    watcher: Watcher,
    sets: Sets,
    /// The guest's end of the agent's port is open.
    open: bool,
}

impl Agent {
    fn control(&mut self, channel: &mut Channel, control: Result<Control, toyos_vdagent::control::Refused>) {
        let Control { id, event, value } = match control {
            Ok(control) => control,
            Err(why) => return say!("vdagent: {why}, ignored"),
        };
        let reply = |event, value| Control { id: PORT_ID, event, value };
        match event {
            Event::DeviceAdd if id == PORT_ID => channel.send_control(reply(Event::PortReady, 1)),
            Event::DeviceAdd => panic!("vdagent: the device added port {id}, and the launch declares port {PORT_ID} alone"),
            Event::ConsolePort => panic!("vdagent: the device made port {id} a console port"),
            Event::DeviceRemove if id == PORT_ID => panic!("vdagent: the device removed port {PORT_ID}"),
            Event::PortOpen if id == PORT_ID && value != 0 => {
                if !self.open {
                    channel.send_control(reply(Event::PortOpen, 1));
                    self.open = true;
                }
            }
            Event::PortOpen if id == PORT_ID => {
                say!("vdagent: the host closed port {PORT_ID}");
                self.bridge.closed();
                self.decoder = Decoder::default();
                if self.open {
                    channel.send_control(reply(Event::PortOpen, 0));
                    self.open = false;
                }
            }
            // Its name is not read: the port is the declared id.
            Event::PortName => {}
            other => say!("vdagent: control {other:?} for port {id}, ignored"),
        }
    }

    /// A message the host sent, named in the log by what it is and never by
    /// its bytes.
    fn host(&mut self, message: Message) {
        match &message {
            Message::Capabilities { request: true, caps } => {
                say!("vdagent: port {PORT_ID} open, the host announces capabilities {caps:#010x}")
            }
            Message::Grab { serial, types } => {
                say!("vdagent: the host grabs the clipboard, serial {serial}, {} type(s)", types.len())
            }
            Message::Clipboard { data, .. } => say!("vdagent: the host's clipboard, {} bytes", data.len()),
            Message::Release => say!("vdagent: the host releases the clipboard"),
            Message::Capabilities { request: false, .. } | Message::Request { .. } => {}
        }
        let effect = self.bridge.host(message);
        self.apply(effect);
    }

    fn apply(&mut self, effect: Effect) {
        match effect {
            Effect::None => {}
            Effect::Guest(text) => {
                if let Some(text) = self.sets.set(text) {
                    self.send(text);
                }
            }
            Effect::Named(named) => say!("vdagent: {named}"),
        }
    }

    fn send(&mut self, text: String) {
        self.watcher.set(text).unwrap_or_else(|why| panic!("vdagent: the compositor's clipboard: {why}"))
    }

    /// Every frame the compositor sent.
    fn watched(&mut self) {
        loop {
            match self.watcher.poll() {
                Ok(Watched::Idle) => return,
                Ok(Watched::Committed) => {}
                Ok(Watched::Answered) => {
                    if let Some(text) = self.sets.answered() {
                        self.send(text);
                    }
                }
                Ok(Watched::Copy(text)) => {
                    say!("vdagent: a guest copy of {} bytes", text.len());
                    self.sets.delivered();
                    let effect = self.bridge.copy(text);
                    self.apply(effect);
                }
                Err(why) => panic!("vdagent: the compositor's clipboard: {why}"),
            }
        }
    }
}

fn main() {
    let Some(claim) = endow::pci_function::<PciDev>(VIRTIO_SERIAL) else {
        say!("vdagent: no virtio-serial function was handed to this program, exiting");
        return;
    };
    let watcher = Watcher::connect(toyos_manifest::clipboard::PORT)
        .unwrap_or_else(|why| panic!("vdagent: the compositor's clipboard: {why}"));
    let mut channel = Channel::open(claim).unwrap_or_else(|why| panic!("vdagent: the function is not driven: {why}"));
    let mut agent = Agent { bridge: Bridge::default(), decoder: Decoder::default(), watcher, sets: Sets::default(), open: false };

    let poller = Poller::new(2);
    poller.watch(channel.claim(), READABLE, TOKEN_CLAIM);
    poller.watch(agent.watcher.conn(), READABLE, TOKEN_WATCHER);
    loop {
        while let Some(control) = channel.control() {
            agent.control(&mut channel, control);
        }
        let Agent { decoder, .. } = &mut agent;
        let mut messages = Vec::new();
        channel.received(|bytes| {
            decoder
                .feed(bytes, |m| messages.push(m))
                .unwrap_or_else(|unframed| panic!("vdagent: {unframed}"))
        });
        for message in messages {
            match message {
                Ok(message) => agent.host(message),
                Err(refusal) => say!("vdagent: {refusal}"),
            }
        }
        if channel.transmit_free() {
            if agent.bridge.owes() {
                channel.transmit(|out| agent.bridge.transmit(out));
            }
        } else if channel.sent().is_some_and(|at| at.elapsed() > TRANSMIT_BOUND) {
            panic!("vdagent: the device has not taken a transmit back within {TRANSMIT_BOUND:?}");
        }

        let timeout = channel.sent().map_or(u64::MAX, |at| {
            let left = (at + TRANSMIT_BOUND).saturating_duration_since(Instant::now());
            u64::try_from(left.as_nanos()).unwrap_or(u64::MAX)
        });
        let mut ready = [false; 2];
        poller.wait(1, timeout, |token| ready[token as usize] = true);
        if ready[TOKEN_CLAIM as usize] {
            channel.take_interrupt();
            poller.watch(channel.claim(), READABLE, TOKEN_CLAIM);
        }
        if ready[TOKEN_WATCHER as usize] {
            agent.watched();
            poller.watch(agent.watcher.conn(), READABLE, TOKEN_WATCHER);
        }
    }
}
