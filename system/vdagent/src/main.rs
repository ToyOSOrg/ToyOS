//! The shared clipboard with QEMU's host: the guest agent of QEMU's
//! `qemu-vdagent` chardev, between the port the virtio-serial driver hands it
//! on [`SERVICE`] and the compositor's clipboard.
//!
//! **What is here is the loop.** The device is the driver's
//! (`/system/bin/virtio-console`), and the port reaches this program as
//! `toyos_vdagent::endpoint` describes; the stream is `toyos-vdagent`'s;
//! every decision about who owns the clipboard and what a transmit owes is
//! `toyos-vdagent-bridge`'s; the compositor's side is `window::watcher`'s. So
//! this program holds the clipboard and no device, and the driver the device
//! and no clipboard.
//!
//! **One port at a time.** The compositor's clipboard is connected to at the
//! first, and kept. A second port while one is served is refused by name; a
//! port that goes, or that sends what the endpoint does not carry, takes its
//! session with it, and the next port starts another. An unframed stream ends
//! the program by name. It is not started again: one that ended says why and
//! stays down for the boot.
//!
//! **What is owed the host is written once what went before has gone**, so
//! a push the host's grab makes stale is not serialized behind it.
//!
//! **No clipboard byte is ever logged**: lengths, serials and refusals only.

use toyos::endow;
use toyos::ipc::{Connection, FrameRx, RxStep, TrySendError, MAX_FRAME_LEN};
use toyos::poller::{Poller, READABLE, WRITABLE};
use toyos::port::Acceptor;
use toyos::say;
use toyos_vdagent::endpoint::{AGENT_BYTES, CLOSED, HOST_BYTES, HOST_CLOSED, HOST_OPENED, SERVICE};
use toyos_vdagent::stream::{Decoder, Message};
use toyos_vdagent::LARGEST_TRANSMIT;
use toyos_vdagent_bridge::sets::Sets;
use toyos_vdagent_bridge::{Bridge, Effect};
use window::watcher::{Watched, Watcher};

/// One declaration of the clipboard's bound, read by the codec and the
/// compositor alike.
const _: () = assert!(toyos_vdagent::MAX_TEXT == window::MAX_CLIPBOARD_BYTES);

const TOKEN_ACCEPT: u64 = 0;
const TOKEN_PORT: u64 = 1;
const TOKEN_WATCHER: u64 = 2;

/// The port the driver handed over, and what of the host's stream and of the
/// agent's is in flight on it.
struct Port {
    conn: Connection,
    rx: Box<FrameRx<{ MAX_FRAME_LEN as usize }>>,
    decoder: Decoder,
    /// The host's closes still to be answered, one [`CLOSED`] each, ahead of
    /// anything else.
    owe_closed: u32,
    /// What [`Bridge::transmit`] wrote, and how much of it has been sent.
    out: Box<[u8]>,
    filled: usize,
    sent: usize,
}

/// Why a port is let go.
enum Gone {
    /// The driver closed it.
    Ended,
    /// It sent what the endpoint does not carry, or refused a send.
    Refused(String),
}

struct Agent {
    bridge: Bridge,
    watcher: Watcher,
    sets: Sets,
    port: Option<Port>,
}

fn main() {
    let acceptor: Acceptor = endow::acceptor(SERVICE).expect("vdagent: its row serves the agent's port");
    let poller = Poller::new(3);
    let mut agent: Option<Agent> = None;
    loop {
        poller.watch(&acceptor, READABLE, TOKEN_ACCEPT);
        if let Some(agent) = &agent {
            if let Some(port) = &agent.port {
                let writing = port.owe_closed > 0 || port.sent < port.filled;
                poller.watch(&port.conn, READABLE | if writing { WRITABLE } else { 0 }, TOKEN_PORT);
            }
            poller.watch(agent.watcher.conn(), READABLE, TOKEN_WATCHER);
        }
        let mut ready = [false; 3];
        poller.wait(1, u64::MAX, |token| ready[token as usize] = true);
        if ready[TOKEN_ACCEPT as usize] {
            accept(&acceptor, &mut agent);
        }
        let Some(agent) = &mut agent else { continue };
        if ready[TOKEN_WATCHER as usize] {
            agent.watched();
        }
        if let Err(gone) = agent.serve() {
            match gone {
                Gone::Ended => say!("vdagent: the driver closed the agent's port"),
                Gone::Refused(why) => say!("vdagent: the agent's port is let go: {why}"),
            }
            agent.bridge.closed();
            agent.port = None;
        }
    }
}

/// A port the driver connected with: served where none is, refused where one
/// is.
fn accept(acceptor: &Acceptor, agent: &mut Option<Agent>) {
    let conn = match acceptor.accept() {
        Ok(conn) => conn,
        Err(why) => return say!("vdagent: a port went before it was accepted: {why:?}"),
    };
    if agent.as_ref().is_some_and(|agent| agent.port.is_some()) {
        return say!("vdagent: a second agent's port is refused: one is served");
    }
    let agent = agent.get_or_insert_with(|| {
        let watcher = Watcher::connect(toyos_manifest::clipboard::PORT)
            .unwrap_or_else(|why| panic!("vdagent: the compositor's clipboard: {why}"));
        Agent { bridge: Bridge::default(), watcher, sets: Sets::default(), port: None }
    });
    agent.port = Some(Port {
        conn,
        rx: Box::new(FrameRx::new()),
        decoder: Decoder::default(),
        owe_closed: 0,
        out: vec![0; LARGEST_TRANSMIT].into_boxed_slice(),
        filled: 0,
        sent: 0,
    });
}

impl Agent {
    /// Everything the port sent, then everything owed it.
    fn serve(&mut self) -> Result<(), Gone> {
        loop {
            let Some(port) = &mut self.port else { return Ok(()) };
            let mut messages = Vec::new();
            match port.rx.pump(&port.conn) {
                RxStep::Frame { msg_type: HOST_OPENED, payload_len: 0, handles: 0 } => {}
                RxStep::Frame { msg_type: HOST_CLOSED, payload_len: 0, handles: 0 } => {
                    say!("vdagent: the host closed its end");
                    port.decoder = Decoder::default();
                    port.owe_closed += 1;
                    port.filled = 0;
                    port.sent = 0;
                    self.bridge.closed();
                }
                RxStep::Frame { msg_type: HOST_BYTES, payload_len, handles: 0 } => port
                    .decoder
                    .feed(port.rx.payload(payload_len), |m| messages.push(m))
                    .unwrap_or_else(|unframed| panic!("vdagent: {unframed}")),
                RxStep::Frame { msg_type, payload_len, handles } => {
                    return Err(Gone::Refused(format!("frame type {msg_type} of {payload_len} bytes and {handles} handles")));
                }
                RxStep::Idle => break,
                RxStep::Eof => return Err(Gone::Ended),
                RxStep::Malformed => return Err(Gone::Refused(format!("a frame wider than {MAX_FRAME_LEN} bytes"))),
            }
            for message in messages {
                match message {
                    Ok(message) => self.host(message),
                    Err(refusal) => say!("vdagent: {refusal}"),
                }
            }
        }
        self.flush()
    }

    /// The close's answer, then what the bridge owes, a frame at a time,
    /// until the driver has no room.
    fn flush(&mut self) -> Result<(), Gone> {
        let Some(port) = &mut self.port else { return Ok(()) };
        let refused = |why: TrySendError| Gone::Refused(format!("it refused a send: {why:?}"));
        while port.owe_closed > 0 {
            match port.conn.try_signal(CLOSED) {
                Ok(()) => port.owe_closed -= 1,
                Err(TrySendError::Full) => return Ok(()),
                Err(why) => return Err(refused(why)),
            }
        }
        loop {
            if port.sent == port.filled {
                if !self.bridge.owes() {
                    return Ok(());
                }
                port.filled = self.bridge.transmit(&mut port.out);
                port.sent = 0;
            }
            let end = port.filled.min(port.sent + MAX_FRAME_LEN as usize);
            match port.conn.try_send_bytes(AGENT_BYTES, &port.out[port.sent..end]) {
                Ok(()) => port.sent = end,
                Err(TrySendError::Full) => return Ok(()),
                Err(why) => return Err(refused(why)),
            }
        }
    }

    /// A message the host sent, named in the log by what it is and never by
    /// its bytes.
    fn host(&mut self, message: Message) {
        match &message {
            Message::Capabilities { request: true, caps } => {
                say!("vdagent: the host announces capabilities {caps:#010x}")
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
