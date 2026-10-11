//! An SSH server, sans-IO: the client's bytes in, bytes for the client and
//! [`Event`]s for the driver out. No socket, no thread and no clock; the
//! randomness is the caller's.
//!
//! **One algorithm set, and nothing else offered**: `curve25519-sha256` with
//! strict key exchange, an `ssh-ed25519` host key,
//! `chacha20-poly1305@openssh.com` both ways, no compression; `publickey`
//! authentication with `ssh-ed25519` keys; `session` channels serving `exec`.
//!
//! **Every byte [`Server::input`] takes is the peer's.** Every length is held
//! to what remains and to a cap named for its field, and what breaks a rule
//! ends the session with a [`Refusal`] naming the field or the rule — never a
//! panic. After a refusal the output holds at most a DISCONNECT, and every
//! later input is refused.
//!
//! **The phases are types.** Before authentication the session is an
//! `auth::PreAuth`, which holds no channel table; the connection layer is made
//! only from the proof a verified signature leaves, and a [`ChannelId`] only by
//! a channel opening in it.

#![forbid(unsafe_code)]

mod auth;
mod base64;
mod connection;
pub mod hostkey;
mod kex;
mod transport;
mod wire;

use ring::rand::SecureRandom;

pub use connection::ChannelId;
pub use hostkey::HostKey;
pub use wire::Refusal;

use auth::PreAuth;
use connection::Session;
use transport::{Incoming, Transport};

/// Message numbers (RFC 4250 §4.1, RFC 8308 §2.3) and disconnect reasons
/// (RFC 4250 §4.2.2).
mod msg {
    pub(crate) const DISCONNECT: u8 = 1;
    pub(crate) const IGNORE: u8 = 2;
    pub(crate) const UNIMPLEMENTED: u8 = 3;
    pub(crate) const DEBUG: u8 = 4;
    pub(crate) const SERVICE_REQUEST: u8 = 5;
    pub(crate) const SERVICE_ACCEPT: u8 = 6;
    pub(crate) const EXT_INFO: u8 = 7;
    pub(crate) const KEXINIT: u8 = 20;
    pub(crate) const NEWKEYS: u8 = 21;
    pub(crate) const KEX_ECDH_INIT: u8 = 30;
    pub(crate) const KEX_ECDH_REPLY: u8 = 31;
    /// The first number of the layers above the transport.
    pub(crate) const USERAUTH_FIRST: u8 = 50;
    pub(crate) const USERAUTH_REQUEST: u8 = 50;
    pub(crate) const USERAUTH_FAILURE: u8 = 51;
    pub(crate) const USERAUTH_SUCCESS: u8 = 52;
    pub(crate) const USERAUTH_PK_OK: u8 = 60;
    pub(crate) const GLOBAL_REQUEST: u8 = 80;
    pub(crate) const REQUEST_FAILURE: u8 = 82;
    pub(crate) const CHANNEL_OPEN: u8 = 90;
    pub(crate) const CHANNEL_OPEN_CONFIRMATION: u8 = 91;
    pub(crate) const CHANNEL_OPEN_FAILURE: u8 = 92;
    pub(crate) const CHANNEL_WINDOW_ADJUST: u8 = 93;
    pub(crate) const CHANNEL_DATA: u8 = 94;
    pub(crate) const CHANNEL_EXTENDED_DATA: u8 = 95;
    pub(crate) const CHANNEL_EOF: u8 = 96;
    pub(crate) const CHANNEL_CLOSE: u8 = 97;
    pub(crate) const CHANNEL_REQUEST: u8 = 98;
    pub(crate) const CHANNEL_SUCCESS: u8 = 99;
    pub(crate) const CHANNEL_FAILURE: u8 = 100;

    pub(crate) const DISCONNECT_PROTOCOL_ERROR: u32 = 2;
    pub(crate) const DISCONNECT_KEY_EXCHANGE_FAILED: u32 = 3;
    pub(crate) const DISCONNECT_NO_MORE_AUTH_METHODS_AVAILABLE: u32 = 14;
}

/// Who may log in with which key: asked at every offer and again with every
/// signature, so a key removed between the two is refused.
pub trait Authorizer {
    fn authorizes(&mut self, user: &str, key: &[u8; 32]) -> bool;
}

/// What the client asked for that the server refused by name, and the
/// session goes on. Every string is the client's, bounded by its field's cap;
/// a user name is any UTF-8, so print it escaped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Declined {
    /// An authentication method other than `publickey`.
    Method(String),
    /// A public key algorithm other than `ssh-ed25519`, RSA's among them.
    KeyAlgorithm(String),
    /// An Ed25519 key the authorizer does not name for this user.
    Key { user: String, fingerprint: String },
    /// A signature that does not verify.
    Signature { user: String },
    /// A channel type other than `session`.
    ChannelType(String),
    /// A channel open past the most a session holds, OpenSSH's `MaxSessions`,
    /// or past the 2^32 numbers it gives.
    ChannelLimit,
    /// A channel request other than one `exec`.
    Request(String),
    /// A global request; none is served.
    GlobalRequest(String),
}

/// What the driver is told.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The client proved it holds a key the authorizer named for `user`.
    Authenticated { user: String },
    /// Run `command` on `channel`. Its bytes are the client's.
    Exec { channel: ChannelId, command: Vec<u8> },
    /// Bytes for the program's standard input. The client's window comes back
    /// only as [`Server::consumed`] says they were taken.
    Data { channel: ChannelId, data: Vec<u8> },
    /// The client will send no more on `channel`.
    Eof { channel: ChannelId },
    /// The client closed `channel`; whatever runs on it is ended.
    Closed { channel: ChannelId },
    /// The client's window grew: [`Server::send`] may take more.
    Writable { channel: ChannelId },
    /// A request refused by name.
    Declined(Declined),
    /// The client ended the session.
    Disconnected,
}

/// The channel is closed, or the session is over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gone;

enum Phase {
    PreAuth(PreAuth),
    Authenticated(Box<Session>),
    Ended,
}

/// One client's session.
pub struct Server<A, R> {
    transport: Transport<R>,
    authorizer: A,
    phase: Phase,
    events: std::collections::VecDeque<Event>,
}

impl<A: Authorizer, R: SecureRandom> Server<A, R> {
    /// A session whose identification line and KEXINIT are already in
    /// [`Server::output`].
    pub fn new(host_key: HostKey, authorizer: A, rng: R) -> Self {
        Self {
            transport: Transport::new(host_key, rng),
            authorizer,
            phase: Phase::PreAuth(PreAuth::new()),
            events: std::collections::VecDeque::new(),
        }
    }

    /// Take the client's next bytes, in any split.
    pub fn input(&mut self, bytes: &[u8]) -> Result<(), Refusal> {
        if matches!(self.phase, Phase::Ended) {
            return Err(Refusal::Ended);
        }
        self.transport.feed(bytes);
        self.process().inspect_err(|refusal| {
            self.phase = Phase::Ended;
            self.transport.refuse(refusal);
        })
    }

    fn process(&mut self) -> Result<(), Refusal> {
        while let Some(incoming) = self.transport.next()? {
            let (payload, session_id) = match incoming {
                Incoming::Payload(payload, session_id) => (payload, session_id),
                Incoming::Disconnect => {
                    self.events.push_back(Event::Disconnected);
                    self.phase = Phase::Ended;
                    return Ok(());
                }
            };
            let mut out = Vec::new();
            let mut events = Vec::new();
            let verified = match &mut self.phase {
                Phase::PreAuth(pre) => pre.handle(&payload, &session_id, &mut self.authorizer, &mut out, &mut events),
                Phase::Authenticated(session) => session.handle(&payload, &mut out, &mut events).map(|()| None),
                Phase::Ended => Err(Refusal::Ended),
            };
            // What was refused on the way to a refusal is the driver's too.
            self.events.extend(events);
            if let Some(verified) = verified? {
                self.transport.authenticated();
                self.events.push_back(Event::Authenticated { user: verified.user().to_string() });
                self.phase = Phase::Authenticated(Box::new(Session::new(verified)));
            }
            self.flush(out)?;
        }
        Ok(())
    }

    fn flush(&mut self, out: Vec<Vec<u8>>) -> Result<(), Refusal> {
        out.into_iter().try_for_each(|payload| self.transport.send(payload))
    }

    /// The next thing the driver is told.
    pub fn poll(&mut self) -> Option<Event> {
        self.events.pop_front()
    }

    /// Every byte for the client so far.
    pub fn output(&mut self) -> Vec<u8> {
        self.transport.take_output()
    }

    fn session(&mut self, act: impl FnOnce(&mut Session, &mut Vec<Vec<u8>>) -> Result<usize, Gone>) -> Result<usize, Gone> {
        let Phase::Authenticated(session) = &mut self.phase else { return Err(Gone) };
        let mut out = Vec::new();
        let done = act(session, &mut out)?;
        if let Err(refusal) = self.flush(out) {
            self.phase = Phase::Ended;
            self.transport.refuse(&refusal);
            return Err(Gone);
        }
        Ok(done)
    }

    /// Send the program's standard output on `channel`, as much as the
    /// client's window takes; the rest waits for [`Event::Writable`].
    pub fn send(&mut self, channel: ChannelId, data: &[u8]) -> Result<usize, Gone> {
        self.session(|session, out| session.send(channel, data, false, out))
    }

    /// [`Server::send`] for standard error.
    pub fn send_stderr(&mut self, channel: ChannelId, data: &[u8]) -> Result<usize, Gone> {
        self.session(|session, out| session.send(channel, data, true, out))
    }

    /// The program on `channel` exited with `status`: say so and close it.
    pub fn exit(&mut self, channel: ChannelId, status: u32) -> Result<(), Gone> {
        self.session(|session, out| session.exit(channel, status, out).map(|()| 0)).map(|_| ())
    }

    /// The program on `channel` took `n` more bytes of its [`Event::Data`];
    /// more than it was given is the driver's bug, and panics.
    pub fn consumed(&mut self, channel: ChannelId, n: usize) -> Result<(), Gone> {
        self.session(|session, out| session.consumed(channel, n, out).map(|()| 0)).map(|_| ())
    }
}
