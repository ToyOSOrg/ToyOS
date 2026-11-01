//! The Wi-Fi supplicant: one radio driver at a time on `radio` and the settings programs on
//! `wifi`, every decision `toyos-wlan-station`'s.
//!
//! **This program holds no rule.** What is here is what the station cannot do and stay pure:
//! the connections, the clock, the kernel's random source and the log.
//!
//! **One pass**: each connection the kernel says is ready gives the station its frames, a few
//! at a time so no peer keeps the others waiting; then every bound that passed; then what the
//! station has to send is sent with `try_send`, a peer that cannot take it ended by name; and
//! the loop sleeps until a connection or the station's next deadline wakes it.
//!
//! **A settings program's frame is read where it was received**, and its buffer wiped once the
//! station has it: a connect carries a passphrase.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use toyos::endow;
use toyos::ipc::{Connection, FrameRx, RxStep};
use toyos::poller::{Poller, READABLE};
use toyos::port::Acceptor;
use toyos::say;
use toyos_wlan_station::{Client, Out, Station};

/// The most settings programs connected at once; a person runs one or two.
const MAX_CLIENTS: usize = 8;

/// The most frames one peer is taken in a pass.
const FRAMES_PER_PASS: usize = 16;

const TOKEN_RADIOS: u64 = 0;
const TOKEN_CLIENTS: u64 = 1;
const TOKEN_RADIO: u64 = 2;
const TOKEN_CLIENT_BASE: u64 = 16;

/// The most watches one pass registers. Each token is handled once a pass: an acceptor's
/// watches itself and the connection it accepted, and the radio and each client's connection
/// itself.
const MAX_WATCHES: u32 = 2 * 2 + 1 + MAX_CLIENTS as u32;

const _: () = assert!(toyos_radio::MAX_PAYLOAD <= toyos::ipc::MAX_FRAME_LEN as usize);
const _: () = assert!(toyos_wifi::MAX_PAYLOAD <= toyos::ipc::MAX_FRAME_LEN as usize);

struct Peer<const MAX: usize> {
    conn: Connection,
    rx: Box<FrameRx<MAX>>,
}

impl<const MAX: usize> Peer<MAX> {
    fn new(conn: Connection) -> Self {
        Self { conn, rx: Box::new(FrameRx::new()) }
    }
}

type RadioPeer = Peer<{ toyos_radio::MAX_PAYLOAD }>;
type ClientPeer = Peer<{ toyos_wifi::MAX_PAYLOAD }>;

/// Why a peer's connection is over, if what it sent ends it.
fn ended(step: RxStep) -> Option<&'static str> {
    match step {
        RxStep::Idle | RxStep::Frame { handles: 0, .. } => None,
        RxStep::Frame { .. } => Some("it sent a handle, which no frame of its protocol carries"),
        RxStep::Eof => Some("its connection ended"),
        RxStep::Malformed => Some("it sent a frame wider than its protocol's"),
    }
}

fn nonce() -> [u8; 32] {
    let mut bytes = [0u8; 32];
    toyos_abi::syscall::random(&mut bytes)
        .unwrap_or_else(|e| panic!("wlan: the kernel's random source refused a nonce: {e:?}"));
    bytes
}

struct Wlan {
    station: Station,
    began: Instant,
    radio: Option<RadioPeer>,
    clients: Vec<Option<(Client, ClientPeer)>>,
    next_client: Client,
}

impl Wlan {
    fn now(&self) -> Duration {
        self.began.elapsed()
    }

    fn accept_radio(&mut self, radios: &Acceptor, poller: &Poller) {
        match radios.accept() {
            Ok(conn) if self.radio.is_none() => {
                poller.watch(&conn, READABLE, TOKEN_RADIO);
                self.radio = Some(Peer::new(conn));
                self.station.radio_connected(self.now());
            }
            Ok(_) => say!("wlan: refused a second radio: this program drives one at a time"),
            Err(why) => say!("wlan: the kernel refused a radio's connection: {why:?}"),
        }
    }

    fn accept_client(&mut self, clients: &Acceptor, poller: &Poller) {
        let conn = match clients.accept() {
            Ok(conn) => conn,
            Err(why) => return say!("wlan: the kernel refused a settings program's connection: {why:?}"),
        };
        let Some(slot) = self.clients.iter().position(Option::is_none) else {
            return say!("wlan: refused a settings program: {MAX_CLIENTS} are connected");
        };
        let client = self.next_client;
        self.next_client += 1;
        poller.watch(&conn, READABLE, TOKEN_CLIENT_BASE + slot as u64);
        self.clients[slot] = Some((client, Peer::new(conn)));
    }

    fn end_radio(&mut self, why: &str) {
        if self.radio.take().is_some() {
            say!("wlan: the radio's connection is over: {why}");
            self.station.radio_gone();
        }
    }

    fn end_client(&mut self, client: Client) {
        if let Some(slot) = self.clients.iter_mut().find(|s| s.as_ref().is_some_and(|(c, _)| *c == client)) {
            if let Some((_, peer)) = slot {
                peer.rx.wipe();
            }
            *slot = None;
            self.station.client_gone(client);
        }
    }

    fn radio_ready(&mut self, poller: &Poller) {
        for _ in 0..FRAMES_PER_PASS {
            let now = self.now();
            let Some(peer) = &mut self.radio else { return };
            let step = peer.rx.pump(&peer.conn);
            if let Some(why) = ended(step) {
                return self.end_radio(why);
            }
            let RxStep::Frame { msg_type, payload_len, .. } = step else { break };
            self.station.radio(msg_type, peer.rx.payload(payload_len), now, nonce);
            if !self.station.has_radio() {
                // The station ended it for what it sent.
                self.radio = None;
                return;
            }
        }
        if let Some(peer) = &self.radio {
            poller.watch(&peer.conn, READABLE, TOKEN_RADIO);
        }
    }

    fn client_ready(&mut self, slot: usize, poller: &Poller) {
        for _ in 0..FRAMES_PER_PASS {
            let now = self.now();
            let Some(Some((client, peer))) = self.clients.get_mut(slot) else { return };
            let client = *client;
            let step = peer.rx.pump(&peer.conn);
            if ended(step).is_some() {
                return self.end_client(client);
            }
            let RxStep::Frame { msg_type, payload_len, .. } = step else { break };
            self.station.request(client, msg_type, peer.rx.payload(payload_len), now);
            peer.rx.wipe();
        }
        if let Some(Some((_, peer))) = self.clients.get(slot) {
            poller.watch(&peer.conn, READABLE, TOKEN_CLIENT_BASE + slot as u64);
        }
    }

    /// What the station has to send, sent.
    fn send(&mut self) {
        loop {
            let outs = self.station.take();
            if outs.is_empty() {
                return;
            }
            for out in outs {
                match out {
                    Out::Radio { msg_type, payload } => {
                        let Some(peer) = &self.radio else { continue };
                        if let Err(why) = peer.conn.try_send_bytes(msg_type, payload.bytes()) {
                            self.end_radio(&format!("it took no frame: {why:?}"));
                        }
                    }
                    // The station said why in its log line.
                    Out::EndRadio(_) => self.radio = None,
                    Out::Answer { client, msg_type, payload } => {
                        let peer = self.clients.iter().flatten().find(|(c, _)| *c == client);
                        if let Some((_, peer)) = peer
                            && let Err(why) = peer.conn.try_send_bytes(msg_type, payload.bytes())
                        {
                            say!("wlan: ending a settings program that took no answer: {why:?}");
                            self.end_client(client);
                        }
                    }
                    Out::EndClient(client) => {
                        say!("wlan: ending a settings program that broke the wifi protocol");
                        self.end_client(client);
                    }
                    Out::Log(line) => say!("{line}"),
                }
            }
        }
    }
}

fn main() {
    // Both ports exist before this process does: a driver's or a settings program's connection
    // is queued on it whether or not this program has reached `accept`.
    let radios = endow::acceptor(toyos_radio::PORT).expect("the manifest declares this program serves `radio`");
    let clients = endow::acceptor(toyos_wifi::PORT).expect("the manifest declares this program serves `wifi`");
    let poller = Poller::new(MAX_WATCHES);
    poller.watch(&radios, READABLE, TOKEN_RADIOS);
    poller.watch(&clients, READABLE, TOKEN_CLIENTS);
    let mut wlan = Wlan {
        station: Station::new(),
        began: Instant::now(),
        radio: None,
        clients: (0..MAX_CLIENTS).map(|_| None).collect(),
        next_client: 0,
    };
    say!("wlan: serving {} and {}", toyos_radio::PORT, toyos_wifi::PORT);
    loop {
        let timeout = match wlan.station.deadline() {
            Some(at) => u64::try_from(at.saturating_sub(wlan.now()).as_nanos()).unwrap_or(u64::MAX),
            None => u64::MAX,
        };
        // A watch left on a connection that has since ended can answer beside its slot's new
        // one: each token is handled once a pass.
        let mut ready = BTreeSet::new();
        poller.wait(1, timeout, |token| {
            ready.insert(token);
        });
        for token in ready {
            match token {
                TOKEN_RADIOS => {
                    wlan.accept_radio(&radios, &poller);
                    poller.watch(&radios, READABLE, TOKEN_RADIOS);
                }
                TOKEN_CLIENTS => {
                    wlan.accept_client(&clients, &poller);
                    poller.watch(&clients, READABLE, TOKEN_CLIENTS);
                }
                TOKEN_RADIO => wlan.radio_ready(&poller),
                token => wlan.client_ready((token - TOKEN_CLIENT_BASE) as usize, &poller),
            }
            wlan.send();
        }
        let now = wlan.now();
        wlan.station.tick(now);
        wlan.send();
    }
}
