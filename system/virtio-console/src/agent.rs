//! QEMU's clipboard channel: the port named [`toyos_vdagent::PORT_NAME`],
//! carried both ways between the device and the clipboard's agent on
//! [`SERVICE`] (`toyos_vdagent::endpoint`).
//!
//! **The handshake is virtio 1.2 §5.3.6.2's**: the host's `PORT_OPEN(1)` is
//! answered with the guest's `PORT_OPEN(1)`, and its `PORT_OPEN(0)` with
//! `PORT_OPEN(0)`; QEMU 11.1.1 then starts a new session with its
//! capabilities, measured. A console port and the removal of the port end the
//! program by name.
//!
//! **Each way waits on its taker rather than dropping**: a host buffer the
//! agent has no room for stays unposted until it has, and the agent's frames
//! are read only into a free transmit buffer. The host's opens and closes are
//! told the agent after the buffers read beside them: the driver's order, since
//! the device gives none between its control and receive queues. What the
//! agent wrote for a session the host has closed is dropped ([`Fence`]). A
//! transmit the device has not taken back within [`TRANSMIT_BOUND`] ends the
//! program: QEMU 11.1.1 takes each at once, measured, and a device holding
//! one holds every message after it.
//!
//! **No clipboard byte is ever logged**: lengths and refusals only.

use std::collections::VecDeque;
use std::time::Duration;

use toyos::endow::{self, EndowError};
use toyos::ipc::{Connection, FrameRx, RxStep, TrySendError, MAX_FRAME_LEN};
use toyos::poller::{Poller, READABLE, WRITABLE};
use toyos::say;
use toyos_vdagent::endpoint::{self, Fence, HOST_BYTES, HOST_CLOSED, HOST_OPENED, SERVICE};
use toyos_virtio_serial::control::{Control, Event};
use toyos_virtio_serial::probe::{Probe, Step};
use toyos_virtio_serial::PORT_ID;

use crate::channel::{Channel, Host};

/// How long a transmit may stay with the device. Policy, far past any time
/// QEMU 11.1.1 took, measured: it took every element at once.
const TRANSMIT_BOUND: Duration = Duration::from_secs(5);

const TOKEN_CLAIM: u64 = 0;
const TOKEN_AGENT: u64 = 1;

struct Agent {
    channel: Channel,
    probe: Probe,
    conn: Connection,
    rx: Box<FrameRx<{ MAX_FRAME_LEN as usize }>>,
    /// The guest's end of the port is open.
    open: bool,
    /// Whether what the agent sends is a closed session's.
    fence: Fence,
    /// The host's opens and closes not yet told, in order, behind any host
    /// buffer the agent has not taken.
    owed: VecDeque<Control>,
    /// The agent had no room for a message.
    blocked: bool,
}

pub fn run(mut channel: Channel, probe: Probe, held: Vec<Control>, poller: Poller) -> ! {
    let conn = match endow::service(SERVICE) {
        Ok(conn) => conn,
        Err(EndowError::NotEndowed) => {
            say!("virtio-console: port {PORT_ID} is the clipboard's, and this image runs no agent for it");
            std::process::exit(0);
        }
        Err(why) => panic!("virtio-console: the clipboard's agent: {why:?}"),
    };
    channel.post_receives();
    say!("virtio-console: carrying the clipboard's channel on port {PORT_ID}");
    let mut agent = Agent {
        channel,
        probe,
        conn,
        rx: Box::new(FrameRx::new()),
        open: false,
        fence: Fence::default(),
        owed: held.into(),
        blocked: false,
    };
    loop {
        agent.controls();
        agent.host();
        agent.channel.transmitted();
        agent.transmit();
        let stuck = agent.channel.oldest().filter(|_| !agent.channel.can_transmit());
        if stuck.is_some_and(|at| at.elapsed() > TRANSMIT_BOUND) {
            panic!("virtio-console: the device has not taken a transmit back within {TRANSMIT_BOUND:?}");
        }

        poller.watch(agent.channel.claim(), READABLE, TOKEN_CLAIM);
        let mut interest = 0;
        if agent.channel.can_transmit() || agent.fence.closing() {
            interest |= READABLE;
        }
        if agent.blocked {
            interest |= WRITABLE;
        }
        if interest != 0 {
            poller.watch(&agent.conn, interest, TOKEN_AGENT);
        }
        let timeout = stuck.map_or(u64::MAX, |at| crate::until(at, TRANSMIT_BOUND));
        let mut interrupted = false;
        poller.wait(1, timeout, |token| interrupted |= token == TOKEN_CLAIM);
        if interrupted {
            agent.channel.take_interrupt();
        }
    }
}

impl Agent {
    /// Every control message the device sent since the last round: the
    /// probe's answered at once, the port's held for their turn.
    fn controls(&mut self) {
        while let Some(received) = self.channel.control() {
            match self.probe.take(received.bytes()) {
                Ok(Step::Reply(reply)) => self.channel.send_control(reply),
                Ok(Step::Driver(control)) => self.owed.push_back(control),
                Ok(Step::Nothing) => {}
                Err(why) => panic!("virtio-console: {why}"),
            }
        }
    }

    /// The host's buffers and then its messages to the agent, while the agent
    /// has room for them.
    fn host(&mut self) {
        self.blocked = false;
        loop {
            let conn = &self.conn;
            let mut blocked = false;
            let step = self.channel.receive(|bytes| match conn.try_send_bytes(HOST_BYTES, bytes) {
                Ok(()) => true,
                Err(TrySendError::Full) => {
                    blocked = true;
                    false
                }
                Err(why) => panic!("virtio-console: the clipboard's agent refused the host's bytes: {why:?}"),
            });
            match step {
                Host::Taken => continue,
                Host::Held => {
                    self.blocked = blocked;
                    return;
                }
                Host::Empty => break,
            }
        }
        while let Some(&control) = self.owed.front() {
            if !self.port(control) {
                self.blocked = true;
                return;
            }
            self.owed.pop_front();
        }
    }

    /// A message about the agent's port, in its turn: `false` where the
    /// agent had no room for what it owes it, to be tried again.
    fn port(&mut self, Control { event, value, .. }: Control) -> bool {
        let open = match event {
            Event::ConsolePort => panic!("virtio-console: the device made the clipboard's port {PORT_ID} a console port"),
            Event::DeviceRemove => panic!("virtio-console: the device removed the clipboard's port {PORT_ID}"),
            Event::PortOpen => value != 0,
            other => {
                say!("virtio-console: control {other:?} for the clipboard's port, ignored");
                return true;
            }
        };
        match self.conn.try_signal(if open { HOST_OPENED } else { HOST_CLOSED }) {
            Ok(()) => {}
            Err(TrySendError::Full) => return false,
            Err(why) => panic!("virtio-console: the clipboard's agent refused the host's open or close: {why:?}"),
        }
        if open != self.open {
            self.channel.send_control(Control { id: PORT_ID, event: Event::PortOpen, value: open.into() });
            self.open = open;
        }
        if !open {
            say!("virtio-console: the host closed the clipboard's port");
            self.fence.host_closed();
        }
        true
    }

    /// The agent's bytes, one frame to each free transmit buffer, past its
    /// fence.
    fn transmit(&mut self) {
        while self.channel.can_transmit() || self.fence.closing() {
            let (msg_type, len) = match self.rx.pump(&self.conn) {
                RxStep::Frame { msg_type, payload_len, handles: 0 } => (msg_type, payload_len),
                RxStep::Frame { msg_type, handles, .. } => {
                    panic!("virtio-console: the clipboard's agent sent frame type {msg_type} with {handles} handles")
                }
                RxStep::Idle => return,
                RxStep::Eof => panic!("virtio-console: the clipboard's agent went"),
                RxStep::Malformed => panic!("virtio-console: the clipboard's agent sent a frame wider than {MAX_FRAME_LEN} bytes"),
            };
            match self.fence.agent(msg_type) {
                Ok(endpoint::Agent::Transmit) => {
                    let payload = self.rx.payload(len);
                    self.channel.transmit(|buffer| {
                        buffer[..len].copy_from_slice(payload);
                        len
                    });
                }
                Ok(endpoint::Agent::Drop | endpoint::Agent::Closed) => {}
                Err(endpoint::Refused::Unknown(msg_type)) => {
                    panic!("virtio-console: the clipboard's agent sent frame type {msg_type}, which it does not send")
                }
                Err(endpoint::Refused::Unasked) => {
                    panic!("virtio-console: the clipboard's agent answered a close the host never made")
                }
            }
        }
    }
}
