//! Every decision the clipboard agent makes between QEMU's vdagent and the
//! compositor: who owns the clipboard, the grab serials, and what each
//! transmit owes. Pure, and host-tested.
//!
//! **Every statement here about what QEMU does is QEMU 11.1.1's behaviour,
//! measured black-box** against its `qemu-vdagent` chardev with a VNC client as
//! the host's clipboard; spice-protocol's `vd_agent.h` defines the messages and
//! leaves a `GRAB`'s serial without a meaning. What the measurement says:
//!
//! - Each session starts when the guest opens its port: the host announces its
//!   capabilities with `request` 1 and starts its serial counter *C* at 0. A
//!   second guest announcement in one session closes the host's end.
//! - A host `GRAB` carries *C*, and *C* then goes up by one. A guest `GRAB`
//!   with serial *g* is kept if and only if *g* ≥ *C*, and a kept one sets
//!   *C* := *g*; a discarded one makes the `CLIPBOARD` or `RELEASE` after it
//!   reach the host as nothing.
//! - A guest `REQUEST` that no grab answers is answered with `CLIPBOARD
//!   [UTF8_TEXT]` and no data.
//! - A kept guest `GRAB` with its `CLIPBOARD` in one element gives the host
//!   the text: a host paste after it is served without a `REQUEST`. A guest
//!   `GRAB` with a `RELEASE` empties the host's clipboard, and the host sends a
//!   `RELEASE` back.
//! - A host paste that reaches the guest as a `REQUEST` is settled by a push's
//!   second message when it falls between the push's two, abandoned by the
//!   host's own grab when one crosses it, and an answer after either reaches
//!   the host as nothing.
//!
//! **So the bridge answers no host `REQUEST`**, and pushes every guest copy
//! with its text: after the push the host holds the text, and no host paste
//! waits on the guest.
//!
//! **Why the serial rule is right.** A crossing is a guest grab *g* and a host
//! grab *s* in flight at once. The host's *s* is its *C*, and it then holds
//! *C* = *s* + 1, so it keeps *g* if and only if *g* > *s*. The bridge keeps
//! *s* if and only if *s* ≥ *g*. Exactly one side wins, and both agree which.
//! Without a crossing, a host grab carries at least the last guest grab the
//! host kept, so the bridge keeps it, and the bridge's next grab carries *s* +
//! 1, the host's *C*. Both sides start each session at 0.
//!
//! **No clipboard byte is ever named**: a [`Named`] carries lengths and
//! numbers only.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(test)]
extern crate std;

use alloc::string::String;
use alloc::vec::Vec;

use toyos_vdagent::encode::Writer;
use toyos_vdagent::stream::Message;
use toyos_vdagent::wire;

/// Who holds the clipboard's current text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Owner {
    None,
    Host,
    /// The guest, and its text, which is never empty.
    Guest(String),
}

/// What an event does to the guest's clipboard, or what it says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    None,
    /// The guest's clipboard becomes this text, empty included.
    Guest(String),
    /// Named in the log, and nothing done.
    Named(Named),
}

/// An event the bridge passes over, by name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Named {
    /// The host announced capabilities without `CLIPBOARD_BY_DEMAND` or
    /// `CLIPBOARD_GRAB_SERIAL`: nothing flows this session.
    HostLacks { caps: u32 },
    /// Capabilities with `request` 0, which the host sends only in reply to a
    /// guest asking for them, and this agent never asks.
    Unasked,
    /// A clipboard message outside a session that flows.
    NoSession,
    /// A host grab older than the guest's last.
    GrabDiscarded { serial: u32, last: u32 },
    /// Host clipboard data that does not become the guest's: `Owner::Host` is
    /// not the owner, the type is not text, or the text is not UTF-8.
    ClipboardRefused { kind: u32, len: usize },
    /// A host `REQUEST`, which owes nothing.
    Request { kind: u32 },
    /// A host `RELEASE` while the host does not own the clipboard.
    ReleaseIgnored,
    /// A grab would carry a serial past `u32::MAX`: the session ends.
    SerialsExhausted,
}

impl core::fmt::Display for Named {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::HostLacks { caps } => write!(
                f,
                "the host's capabilities {caps:#x} lack CLIPBOARD_BY_DEMAND or CLIPBOARD_GRAB_SERIAL: \
                 nothing flows this session"
            ),
            Self::Unasked => write!(f, "host capabilities with request 0, which this agent never asked for, refused"),
            Self::NoSession => write!(f, "a clipboard message outside a session that flows, ignored"),
            Self::GrabDiscarded { serial, last } => {
                write!(f, "a host grab with serial {serial}, older than the guest's {last}, discarded")
            }
            Self::ClipboardRefused { kind, len } => write!(
                f,
                "host clipboard data of type {kind} and {len} bytes refused: the host does not own the \
                 clipboard, or it is not UTF-8 text"
            ),
            Self::Request { kind } => write!(f, "a host request for type {kind}, which owes nothing"),
            Self::ReleaseIgnored => write!(f, "a host release while the host does not own the clipboard, ignored"),
            Self::SerialsExhausted => write!(f, "the grab serials passed u32::MAX: this session ends"),
        }
    }
}

/// A push: a guest grab with its text or with a `RELEASE`, and the serial
/// assigned at its copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Push {
    Text { serial: u32 },
    Release { serial: u32 },
}

/// What the next transmit carries. Each kind at most once, and a `REQUEST`
/// never with a push.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Owed {
    capabilities: bool,
    request: bool,
    push: Option<Push>,
}

/// Where the session stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Session {
    /// The port is closed, or the host has not announced itself.
    Closed,
    /// The host announced itself and nothing flows until it does again.
    Silent,
    /// `last` is the serial of the guest's last grab in this session, and
    /// `next` what its next grab carries; never wrapped.
    Open { last: Option<u32>, next: u64 },
}

pub struct Bridge {
    owner: Owner,
    session: Session,
    owed: Owed,
}

impl Default for Bridge {
    fn default() -> Self {
        Self { owner: Owner::None, session: Session::Closed, owed: Owed::default() }
    }
}

impl Bridge {
    pub fn owner(&self) -> &Owner {
        &self.owner
    }

    /// Whether a transmit is owed.
    pub fn owes(&self) -> bool {
        self.owed != Owed::default()
    }

    /// A message the host sent.
    pub fn host(&mut self, message: Message) -> Effect {
        match message {
            Message::Capabilities { request: true, caps } => self.session(caps),
            Message::Capabilities { request: false, .. } => Effect::Named(Named::Unasked),
            Message::Grab { serial, types } => self.host_grab(serial, &types),
            Message::Clipboard { kind, data } => self.host_clipboard(kind, data),
            Message::Request { kind } => Effect::Named(Named::Request { kind }),
            Message::Release if self.owner == Owner::Host => {
                self.owner = Owner::None;
                Effect::Guest(String::new())
            }
            Message::Release => Effect::Named(Named::ReleaseIgnored),
        }
    }

    /// A new session: the bridge answers with its capabilities once, and
    /// pushes the guest's copy or asks for the host's text.
    fn session(&mut self, caps: u32) -> Effect {
        self.owed = Owed { capabilities: true, ..Owed::default() };
        let wanted = wire::CAP_CLIPBOARD_BY_DEMAND | wire::CAP_CLIPBOARD_GRAB_SERIAL;
        if caps & wanted != wanted {
            self.session = Session::Silent;
            return Effect::Named(Named::HostLacks { caps });
        }
        self.session = Session::Open { last: None, next: 0 };
        match self.owner {
            Owner::Guest(_) => self.push(false),
            Owner::None | Owner::Host => {
                self.owner = Owner::Host;
                self.owed.request = true;
                Effect::None
            }
        }
    }

    fn host_grab(&mut self, serial: u32, types: &[u32]) -> Effect {
        let Session::Open { last, next } = &mut self.session else {
            return Effect::Named(Named::NoSession);
        };
        if let Some(last) = *last {
            if serial < last {
                return Effect::Named(Named::GrabDiscarded { serial, last });
            }
        }
        *next = (*next).max(serial as u64 + 1);
        // QEMU would discard an unsent push: its serial is at most this one.
        self.owed.push = None;
        self.owner = Owner::Host;
        self.owed.request = types.contains(&wire::UTF8_TEXT);
        Effect::Guest(String::new())
    }

    fn host_clipboard(&mut self, kind: u32, data: Vec<u8>) -> Effect {
        let len = data.len();
        if self.owner != Owner::Host || kind != wire::UTF8_TEXT {
            return Effect::Named(Named::ClipboardRefused { kind, len });
        }
        match String::from_utf8(data) {
            Ok(text) => Effect::Guest(text),
            Err(_) => Effect::Named(Named::ClipboardRefused { kind, len }),
        }
    }

    /// The guest copied `text`, empty for a copy of nothing.
    pub fn copy(&mut self, text: String) -> Effect {
        self.owed.request = false;
        let empty = text.is_empty();
        self.owner = if empty { Owner::None } else { Owner::Guest(text) };
        self.push(empty)
    }

    /// The owner's push, on the next serial, where the session flows.
    fn push(&mut self, empty: bool) -> Effect {
        let Session::Open { last, next } = &mut self.session else {
            return Effect::None;
        };
        let Ok(serial) = u32::try_from(*next) else {
            self.session = Session::Silent;
            self.owed.push = None;
            return Effect::Named(Named::SerialsExhausted);
        };
        *last = Some(serial);
        *next += 1;
        self.owed.push = Some(if empty { Push::Release { serial } } else { Push::Text { serial } });
        Effect::None
    }

    /// The host closed its end: the serials go, the owner stays, and nothing is
    /// owed until its next announcement.
    pub fn closed(&mut self) {
        self.session = Session::Closed;
        self.owed = Owed::default();
    }

    /// Write what is owed into `out`, in order — capabilities, `REQUEST`, the
    /// push — and owe nothing more. Answers the bytes written.
    ///
    /// # Panics
    /// `out` shorter than what is owed: the caller sizes it
    /// [`toyos_vdagent::LARGEST_TRANSMIT`].
    pub fn transmit(&mut self, out: &mut [u8]) -> usize {
        let owed = core::mem::take(&mut self.owed);
        let mut writer = Writer::new(out);
        if owed.capabilities {
            writer.capabilities();
        }
        if owed.request {
            writer.request();
        }
        match owed.push {
            None => {}
            Some(Push::Text { serial }) => {
                let Owner::Guest(text) = &self.owner else {
                    unreachable!("a text push is owed only while the guest owns the clipboard")
                };
                writer.grab(serial);
                writer.clipboard(text.as_bytes());
            }
            Some(Push::Release { serial }) => {
                writer.grab(serial);
                writer.release();
            }
        }
        writer.len()
    }
}

#[cfg(test)]
mod tests;
