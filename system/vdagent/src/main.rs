//! The shared clipboard with QEMU's host: the guest agent of QEMU's
//! `qemu-vdagent` chardev, between the virtio-serial function it is on and
//! the compositor's clipboard.
//!
//! **What is here is the loop and the device's handshake.** The function is
//! `toyos-virtio-serial-claim`'s and which port is the agent's is
//! `toyos_virtio_serial::probe`'s; the stream is `toyos-vdagent`'s; every
//! decision about who owns the clipboard and what a transmit owes is
//! `toyos-vdagent-bridge`'s; the compositor's side is `window::watcher`'s.
//!
//! **The handshake is virtio 1.2 §5.3.6.2's**: `DEVICE_READY`, each port the
//! device adds answered with `PORT_READY`, and the agent's port — the one the
//! device names [`PORT_NAME`] — opened once the host's `PORT_OPEN(1)` has
//! come. One process runs per virtio-serial function the kernel does not
//! drive, holding its claim; a function none of whose ports is the agent's is
//! another driver's, and this program ends with exit 0, which hands it to the
//! next row that drives one, before it connects to the compositor's
//! clipboard. The host's `PORT_OPEN(0)` is answered with the agent's
//! `PORT_OPEN(0)`, and its `PORT_OPEN(1)` with `PORT_OPEN(1)`: QEMU 11.1.1 then
//! starts a new session with its capabilities, measured. A console port, the
//! removal of the agent's port, a device that misnames its ports, an
//! unframed stream and a used ring the device is not believed on end the
//! program by name. It is not started again: one that ended says why and stays
//! down for the boot.
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
use toyos_manifest::device::PciMatch;
use toyos_vdagent::stream::{Decoder, Message};
use toyos_vdagent::{LARGEST_TRANSMIT, PORT_NAME};
use toyos_vdagent_bridge::sets::Sets;
use toyos_vdagent_bridge::{Bridge, Effect};
use toyos_virtio_serial::control::{Control, Event};
use toyos_virtio_serial::probe::{Probe, Step, Verdict};
use toyos_virtio_serial::PORT_ID;
use toyos_virtio_serial_claim::{Channel, Shape};
use window::watcher::{Watched, Watcher};

/// One declaration of the clipboard's bound, read by the codec and the
/// compositor alike.
const _: () = assert!(toyos_vdagent::MAX_TEXT == window::MAX_CLIPBOARD_BYTES);

/// A communication controller of class `07:80:00`, which virtio 1.2 §5.3.1
/// numbers `0x1040 + 3`: the matcher its row drives, and the label its claim
/// is endowed under.
const VIRTIO_SERIAL: PciMatch = PciMatch::new([0x07, 0x80, 0x00], Some(PciId { vendor: 0x1af4, device: 0x1043 }));

/// How long a transmit may stay with the device. Policy, far past any time
/// QEMU 11.1.1 took, measured: it took every element at once.
const TRANSMIT_BOUND: Duration = Duration::from_secs(5);

/// The agent's port: sixteen receive buffers of a page, and one transmit, of
/// every message owed, in one element.
const SHAPE: Shape = Shape { receive: Some((16, 4096)), transmit: (1, LARGEST_TRANSMIT) };

/// How long the device may take to name its ports. Policy, far past what QEMU
/// 11.1.1 takes: it adds and names them at the driver's first messages.
const PROBE_BOUND: Duration = Duration::from_secs(5);

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
    /// A control message about a port the device added, the agent's to answer.
    fn control(&mut self, channel: &mut Channel, Control { id, event, value }: Control) {
        let reply = |event, value| Control { id: PORT_ID, event, value };
        match event {
            Event::ConsolePort => panic!("vdagent: the device made port {id} a console port"),
            Event::DeviceRemove => panic!("vdagent: the device removed port {PORT_ID}"),
            _ if id != PORT_ID => {}
            Event::PortOpen if value != 0 => {
                if !self.open {
                    channel.send_control(reply(Event::PortOpen, 1));
                    self.open = true;
                }
            }
            Event::PortOpen => {
                say!("vdagent: the host closed port {PORT_ID}");
                self.bridge.closed();
                self.decoder = Decoder::default();
                if self.open {
                    channel.send_control(reply(Event::PortOpen, 0));
                    self.open = false;
                }
            }
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
    let claim: PciDev = endow::claim(&VIRTIO_SERIAL.to_string())
        .expect("vdagent: its row drives a function, and it is started holding the function's claim");
    let mut channel =
        Channel::open(claim, SHAPE, "vdagent").unwrap_or_else(|why| panic!("vdagent: the function is not driven: {why}"));
    let poller = Poller::new(2);
    let mut probe = Probe::new(PORT_NAME);
    let mut held = identify(&mut channel, &mut probe, &poller);
    let watcher = Watcher::connect(toyos_manifest::clipboard::PORT)
        .unwrap_or_else(|why| panic!("vdagent: the compositor's clipboard: {why}"));
    let mut agent = Agent { bridge: Bridge::default(), decoder: Decoder::default(), watcher, sets: Sets::default(), open: false };
    for control in held.drain(..) {
        agent.control(&mut channel, control);
    }

    poller.watch(channel.claim(), READABLE, TOKEN_CLAIM);
    poller.watch(agent.watcher.conn(), READABLE, TOKEN_WATCHER);
    loop {
        while let Some(received) = channel.control() {
            match probe.take(received.bytes()) {
                Ok(Step::Reply(reply)) => channel.send_control(reply),
                Ok(Step::Driver(control)) => agent.control(&mut channel, control),
                Ok(Step::Nothing) => {}
                Err(why) => panic!("vdagent: {why}"),
            }
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
        channel.transmitted();
        if channel.can_transmit() {
            if agent.bridge.owes() {
                channel.transmit(|out| agent.bridge.transmit(out));
            }
        } else if channel.oldest().is_some_and(|at| at.elapsed() > TRANSMIT_BOUND) {
            panic!("vdagent: the device has not taken a transmit back within {TRANSMIT_BOUND:?}");
        }

        let timeout = channel.oldest().map_or(u64::MAX, |at| {
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

/// Read the device's control messages until a port carries the agent's name,
/// and answer the messages for it that came with them; or end with exit 0
/// where none does: the function is another driver's.
fn identify(channel: &mut Channel, probe: &mut Probe<'_>, poller: &Poller) -> Vec<Control> {
    let deadline = Instant::now() + PROBE_BOUND;
    let mut held = Vec::new();
    loop {
        while let Some(received) = channel.control() {
            match probe.take(received.bytes()) {
                Ok(Step::Reply(reply)) => channel.send_control(reply),
                Ok(Step::Driver(control)) => held.push(control),
                Ok(Step::Nothing) => {}
                Err(why) => panic!("vdagent: {why}"),
            }
        }
        match probe.verdict() {
            Verdict::Ours => return held,
            Verdict::Foreign(names) => {
                say!("vdagent: the function's ports are {names:?}, none {PORT_NAME}; another row drives it");
                std::process::exit(0);
            }
            Verdict::Probing => {}
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            say!("vdagent: the function named no port {PORT_NAME} in {PROBE_BOUND:?}; another row drives it");
            std::process::exit(0);
        }
        poller.watch(channel.claim(), READABLE, TOKEN_CLAIM);
        poller.wait(1, u64::try_from(left.as_nanos()).unwrap_or(u64::MAX), |_| {});
        channel.take_interrupt();
    }
}
