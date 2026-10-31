//! Key events down the surface tree.
//!
//! A **surface** is something with a screen and a keyboard: the compositor's
//! windows, `terminal`, `console`. Its owner reads key transitions
//! from whatever is above it and decides what they mean — which for a terminal
//! is bytes on its child's stdin, through a [`toyos_keymap::Translator`] it
//! owns alone.
//!
//! A child that wants the transitions themselves rather than the bytes asks
//! for them here. `locale detect` is the reason this exists: it identifies a
//! keyboard by which HID usage a *labelled* key reports, so the one thing it
//! must not be handed is the current layout's opinion of that key. The
//! keyboard device is claimed exclusively, and under a desktop or a console
//! the surface owner holds it, so before this the wizard refused to run at all
//! wherever anyone would actually run it.
//!
//! Three things travel this channel and nothing else:
//!
//! - **The grab.** A client asks for raw keys; the host answers granted or
//!   refused, and while a grab is held the surface's own translation stops.
//!   The answer is not optional — a client that assumed it had the keys would
//!   wait forever for events going somewhere else.
//! - **A layout change.** [`LAYOUT_CONFIG`] is the layout, and the message
//!   carries no name: it says only that the file changed, so no translator can
//!   hold an opinion that disagrees with it.
//! - **The foreground line.** A shell hands the surface a `MANAGE`-only
//!   handle to each process of the line it waits for ([`Foreground`]), and
//!   the surface turns Ctrl+C into their interrupt ([`Host::interrupt`]): a
//!   quit, which reaches everything below each and kills what does not
//!   listen. Only the line set most recently is asked, so a shell nested on
//!   another's line is asked only while it runs nothing of its own.
//!
//! **The channel is a port, not a name.** A surface makes its own — one per
//! terminal, one per console — and puts the connector in the namespace it
//! builds for the shell it spawns. A wizard three processes below a terminal
//! still finds it, because the namespace is inherited down the same chain the
//! environment used to be; nothing outside that chain can name it, and nothing
//! in it can name a service its parent did not pass.

use alloc::vec::Vec;

use toyos_abi::handle::Rights;
use toyos_abi::input::RawKeyEvent;
use toyos_abi::quit::Reason;
use toyos_abi::syscall::{self, MAX_TRANSFER_HANDLES};
use toyos_abi::RawHandle;

use crate::ipc::{self, Connection, FrameRx, RxStep, TrySendError};
use crate::OwnedHandle;
use crate::poller::{Poller, READABLE};
use crate::port::Acceptor;
use crate::AsHandle;

/// What a surface is called in the namespace of everything below it.
///
/// One name, not one per host: a name in a private namespace does not have to
/// be unique machine-wide, and making it so is what needed a pid in it.
pub const SERVICE: &str = "surface";

/// The file that says which keyboard layout this machine uses.
///
/// One path, read by every translator and written by `locale` alone. A machine
/// setting, so it is in `/config` and in no user's home.
pub const LAYOUT_CONFIG: &str = "/config/keyboard-layout";

/// SAFETY: `RawKeyEvent` is `#[repr(C)]` and two `u8`s, so it has no padding
/// and no pointers, and every bit pattern of both fields is a valid value —
/// which is what makes reinterpreting bytes a peer chose sound.
unsafe impl ipc::IpcPayload for RawKeyEvent {}

/// Client → host: send me key transitions instead of translating them.
pub const MSG_GRAB_KEYS: u32 = 1;
/// Either direction: [`LAYOUT_CONFIG`] changed, re-read it.
pub const MSG_LAYOUT_CHANGED: u32 = 2;
/// Host → client: the keys are yours until you disconnect.
pub const MSG_GRAB_GRANTED: u32 = 3;
/// Host → client: another client on this surface already has them.
pub const MSG_GRAB_REFUSED: u32 = 4;
/// Host → client: one transition, payload [`RawKeyEvent`].
pub const MSG_KEY: u32 = 5;
/// Client → host: the processes of the line this client waits for, at most
/// [`MAX_TRANSFER_HANDLES`] handles each carrying `MANAGE`, payload a `u32`:
/// [`LINE_REPLACE`] for a new line, none for no line, or [`LINE_ADD`] for more
/// of the same one.
pub const MSG_FOREGROUND: u32 = 6;
pub const LINE_REPLACE: u32 = 0;
pub const LINE_ADD: u32 = 1;

/// How many processes one client's line holds. Policy on the host's handle
/// table, which a client's frames would otherwise fill: a client whose line
/// grows past it is dropped, and a shell hands over its line's first this
/// many, so a Ctrl+C reaches a longer pipeline's first processes alone.
pub const MAX_LINE: usize = 64;

/// Whether a line holding `held` processes takes `added` more.
fn line_takes(held: usize, added: usize) -> bool {
    held + added <= MAX_LINE
}

/// How many clients one surface will hold at once.
///
/// Policy, not physics. A surface's clients are the programs running under it
/// that want raw keys, and outside a test that is one wizard at a time; four
/// is room for a shell pipeline that spawned several. Past it a connection is
/// accepted and closed, which the client sees as [`GrabError::HostGone`] —
/// never a queue, because a client waiting behind three others for keys the
/// user is pressing now would be worse than being told no.
pub const MAX_CLIENTS: usize = 4;

/// How a surface owner tells its clients apart in a log line.
///
/// **The connection's own handle, in this process's table.** A peer used to be
/// named by the pid the kernel reported at accept; the kernel does not assert
/// that any more, and a client's own claim about itself is a claim. A handle
/// carries a generation and a closed slot is reissued at the next one, so this
/// names one connection for as long as its holder does not itself point the
/// slot somewhere else — and it designates nothing anywhere else, because a
/// handle is a slot in one table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClientId(RawHandle);

impl core::fmt::Display for ClientId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.0 .0)
    }
}

/// Something the surface owner should log or act on.
#[derive(Clone, Copy, Debug)]
pub enum Notice {
    /// A client says [`LAYOUT_CONFIG`] changed. Re-read it, and pass the same
    /// message on to whatever is above and below this surface.
    LayoutChanged,
    /// This client now takes the key transitions the surface would have
    /// translated.
    Grabbed { client: ClientId },
    /// The client holding the grab has gone; translation resumes.
    Released { client: ClientId },
    /// A peer was dropped, and why — for the log line that names it.
    Dropped { client: ClientId, why: &'static str },
    /// This client now holds a foreground line, or none.
    Foreground { client: ClientId, held: bool },
}

/// What [`Host::deliver`] did with a transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    /// Nobody holds the grab. The surface translates it as usual.
    NotGrabbed,
    /// Handed to the client holding the grab.
    Sent,
}

struct Peer {
    conn: Connection,
    id: ClientId,
    rx: FrameRx<4>,
    /// The processes of its foreground line, which it set as [`Lines`]' turn says.
    line: Vec<OwnedHandle>,
}

/// Which client's line is the foreground: the one set most recently of those
/// that hold one.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lines {
    turns: [Option<u64>; MAX_CLIENTS],
    next: u64,
}

impl Lines {
    /// Client `slot` now holds a line (`held`) or none, set by a new frame
    /// when `fresh`, or added to when not.
    pub fn set(&mut self, slot: usize, held: bool, fresh: bool) {
        match (held, fresh) {
            (false, _) => self.turns[slot] = None,
            (true, true) => {
                self.turns[slot] = Some(self.next);
                self.next += 1;
            }
            (true, false) => {
                if self.turns[slot].is_none() {
                    self.set(slot, true, true);
                }
            }
        }
    }

    /// The slot whose line is the foreground, if any holds one.
    pub fn foreground(&self) -> Option<usize> {
        (0..MAX_CLIENTS).filter_map(|i| Some((i, self.turns[i]?))).max_by_key(|&(_, turn)| turn).map(|(i, _)| i)
    }
}

/// The server side: a surface serving key transitions to its children.
pub struct Host {
    acceptor: Acceptor,
    peers: [Option<Peer>; MAX_CLIENTS],
    /// Index into `peers` of the client taking transitions.
    grab: Option<usize>,
    lines: Lines,
    /// One slot per peer, so a drop decided inside `deliver` cannot be lost
    /// before the caller next asks for notices. A peer is dropped once, and
    /// its slot is written at that moment and read by the next [`Host::poll`].
    drops: [Option<(ClientId, &'static str)>; MAX_CLIENTS],
    pending: [Option<Notice>; MAX_CLIENTS],
}

impl Host {
    /// Serve from a port the caller made, whose connector it puts in the
    /// namespace of every child that is to reach this surface.
    pub fn serve(acceptor: Acceptor) -> Self {
        Self {
            acceptor,
            peers: [const { None }; MAX_CLIENTS],
            grab: None,
            lines: Lines::default(),
            drops: [const { None }; MAX_CLIENTS],
            pending: [const { None }; MAX_CLIENTS],
        }
    }

    /// The acceptor, for the caller's poller.
    pub fn acceptor_handle(&self) -> crate::BorrowedHandle<'_> {
        self.acceptor.as_handle()
    }

    /// Every connected client, for the caller's poller.
    pub fn client_handles(&self) -> impl Iterator<Item = crate::BorrowedHandle<'_>> + '_ {
        self.peers.iter().flatten().map(|p| p.conn.as_handle())
    }

    /// Widest handle set a host adds to its owner's poller.
    pub const POLL_HANDLES: u32 = 1 + MAX_CLIENTS as u32;

    pub fn grabbed(&self) -> bool {
        self.grab.is_some()
    }

    /// Accept one queued connection. Call when the acceptor reads ready.
    ///
    /// Accept and the first frame are two events: nothing is read here, and a
    /// client that connects and then says nothing costs a slot and no more.
    pub fn accept(&mut self) {
        let Ok(conn) = self.acceptor.accept() else {
            return;
        };
        let id = ClientId(conn.as_handle().raw());
        let Some(slot) = self.peers.iter().position(|p| p.is_none()) else {
            // Dropping the connection is the refusal: the client's next read
            // sees the hang-up.
            self.note(Notice::Dropped {
                client: id,
                why: "this surface already holds as many input clients as it will",
            });
            return;
        };
        self.peers[slot] = Some(Peer { conn, id, rx: FrameRx::new(), line: Vec::new() });
    }

    /// Read what every client has sent, and hand back one notice.
    ///
    /// Call until it returns `None`. Grab requests are answered here and are
    /// not notices in themselves — [`Notice::Grabbed`] reports the one that
    /// succeeded.
    pub fn poll(&mut self) -> Option<Notice> {
        for i in 0..MAX_CLIENTS {
            if let Some((client, why)) = self.drops[i].take() {
                return Some(Notice::Dropped { client, why });
            }
            if let Some(notice) = self.pending[i].take() {
                return Some(notice);
            }
        }
        for i in 0..MAX_CLIENTS {
            let Some(peer) = &mut self.peers[i] else { continue };
            let (client, step) = (peer.id, peer.rx.pump(&peer.conn));
            match step {
                RxStep::Idle => {}
                RxStep::Eof => {
                    let held_the_grab = self.grab == Some(i);
                    self.close(i);
                    if held_the_grab {
                        return Some(Notice::Released { client });
                    }
                }
                RxStep::Malformed => {
                    self.close(i);
                    return Some(Notice::Dropped {
                        client,
                        why: "its frame is not one this protocol can produce",
                    });
                }
                RxStep::Frame { msg_type: MSG_FOREGROUND, payload_len: 4, .. } => {
                    let how = u32::from_ne_bytes(peer.rx.payload(4).try_into().expect("four bytes"));
                    let line = peer.rx.take_batch();
                    let n = line.len();
                    let held = if how == LINE_ADD { peer.line.len() } else { 0 };
                    if !line_takes(held, n) {
                        drop(line.collect::<Vec<_>>());
                        self.close(i);
                        return Some(Notice::Dropped { client, why: "its foreground line grew past what a surface holds" });
                    }
                    match how {
                        LINE_REPLACE => peer.line = line.collect(),
                        LINE_ADD => peer.line.extend(line),
                        _ => {
                            drop(line.collect::<Vec<_>>());
                            self.close(i);
                            return Some(Notice::Dropped { client, why: "it set its foreground line in no way this protocol has" });
                        }
                    }
                    let held = !peer.line.is_empty();
                    self.lines.set(i, held, how == LINE_REPLACE);
                    return Some(Notice::Foreground { client, held });
                }
                RxStep::Frame { msg_type, .. } => {
                    if let Some(notice) = self.frame(i, client, msg_type) {
                        return Some(notice);
                    }
                }
            }
        }
        None
    }

    fn frame(&mut self, i: usize, client: ClientId, msg_type: u32) -> Option<Notice> {
        match msg_type {
            MSG_GRAB_KEYS => {
                let free = self.grab.is_none();
                let answer = if free { MSG_GRAB_GRANTED } else { MSG_GRAB_REFUSED };
                let taken = self.peers[i]
                    .as_ref()
                    .expect("the slot this frame came off")
                    .conn
                    .try_signal(answer)
                    .is_ok();
                if !taken {
                    self.close(i);
                    return Some(Notice::Dropped {
                        client,
                        why: "it would not take the answer to its own request",
                    });
                }
                if free {
                    self.grab = Some(i);
                    return Some(Notice::Grabbed { client });
                }
                None
            }
            MSG_LAYOUT_CHANGED => Some(Notice::LayoutChanged),
            _ => {
                self.close(i);
                Some(Notice::Dropped {
                    client,
                    why: "it sent a message this surface does not serve",
                })
            }
        }
    }

    /// Hand one transition to the client holding the grab.
    ///
    /// [`Delivery::NotGrabbed`] is the caller's cue to translate it itself.
    pub fn deliver(&mut self, event: RawKeyEvent) -> Delivery {
        let Some(i) = self.grab else {
            return Delivery::NotGrabbed;
        };
        let peer = self.peers[i].as_ref().expect("the grab names a live peer");
        let (client, sent) = (peer.id, peer.conn.try_send(MSG_KEY, &event));
        match sent {
            Ok(()) => Delivery::Sent,
            other => {
                self.refused(i, client, other, "it would not take a key event it asked for");
                Delivery::NotGrabbed
            }
        }
    }

    /// Ask the processes of the foreground line to quit for an interrupt, as
    /// Ctrl+C does: `false` when no client holds a line, the caller's cue to
    /// pass the key on as it is. A process that has not read the interrupt
    /// before is killed by this one, which is what a second Ctrl+C does.
    pub fn interrupt(&mut self) -> bool {
        let Some(i) = self.lines.foreground() else { return false };
        let peer = self.peers[i].as_ref().expect("a line is held by a live peer");
        for process in &peer.line {
            // A process already ended answers `Ok`; a handle that is no
            // process, or carries no `MANAGE`, is the client's error and
            // asks nothing.
            let _ = syscall::process_quit(process.raw(), Reason::Interrupt);
        }
        true
    }

    /// Tell every client that [`LAYOUT_CONFIG`] changed.
    pub fn notify_layout(&mut self) {
        for i in 0..MAX_CLIENTS {
            let Some(peer) = &self.peers[i] else { continue };
            let (client, sent) = (peer.id, peer.conn.try_signal(MSG_LAYOUT_CHANGED));
            self.refused(i, client, sent, "it would not take a layout notification");
        }
    }

    /// Close a peer a write did not reach, and decide whether that is worth
    /// reporting.
    ///
    /// **A peer that has gone is not a peer at fault.** A client sends
    /// [`MSG_LAYOUT_CHANGED`] and exits, and the broadcast that follows lands
    /// on its closed pipe — the write fails with a syscall error, which is
    /// what a hang-up looks like from this side. Only a *short* write is a
    /// fault: it means the pipe is full, so the peer has a backlog of messages
    /// it asked for and never read, and there is no way to retract the half a
    /// frame the kernel took.
    fn refused(
        &mut self,
        i: usize,
        client: ClientId,
        sent: Result<(), TrySendError>,
        why: &'static str,
    ) {
        match sent {
            Ok(()) => {}
            Err(TrySendError::Syscall(_)) => self.close(i),
            Err(TrySendError::Full) | Err(TrySendError::TooLarge) => {
                self.close(i);
                self.drops[i] = Some((client, why));
            }
        }
    }

    fn close(&mut self, i: usize) {
        self.peers[i] = None;
        self.lines.set(i, false, false);
        if self.grab == Some(i) {
            self.grab = None;
        }
    }

    fn note(&mut self, notice: Notice) {
        if let Some(slot) = self.pending.iter().position(|n| n.is_none()) {
            self.pending[slot] = Some(notice);
        }
    }
}

/// Why a client could not take the keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrabError {
    /// This process has no surface: it was given no `surface` connector, or
    /// the surface that made the port has exited.
    HostGone,
    /// Another client on the same surface already holds them.
    Busy,
    /// The host answered the request with something else.
    Protocol(u32),
}

impl core::fmt::Display for GrabError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::HostGone => write!(f, "the surface hosting this program is not answering"),
            Self::Busy => write!(f, "something else on this surface already has the keyboard"),
            Self::Protocol(t) => write!(f, "the surface answered with message type {t}"),
        }
    }
}

/// The client side: key transitions from this program's surface.
pub struct Keys {
    conn: Connection,
    poller: Poller,
}

impl Keys {
    /// Ask this process's surface for raw key transitions.
    pub fn grab() -> Result<Self, GrabError> {
        let conn = crate::endow::service(SERVICE).map_err(|_| GrabError::HostGone)?;
        conn.signal(MSG_GRAB_KEYS).map_err(|_| GrabError::HostGone)?;
        let frame = FrameRx::<0>::new().recv(&conn).map_err(|_| GrabError::HostGone)?;
        match frame.msg_type {
            MSG_GRAB_GRANTED => Ok(Self { conn, poller: Poller::new(1) }),
            MSG_GRAB_REFUSED => Err(GrabError::Busy),
            other => Err(GrabError::Protocol(other)),
        }
    }

    /// The next transition, or `None` when `timeout_nanos` passes.
    ///
    /// A layout change arriving on this channel is consumed and not reported:
    /// a client holding the keys is reading usages, which no layout moves.
    pub fn next(&mut self, timeout_nanos: u64) -> Option<RawKeyEvent> {
        loop {
            self.poller.watch(&self.conn, READABLE, 0);
            let mut ready = false;
            self.poller.wait(1, timeout_nanos, |_| ready = true);
            if !ready {
                return None;
            }
            let mut rx = FrameRx::<{ core::mem::size_of::<RawKeyEvent>() }>::new();
            let frame = rx.recv(&self.conn).ok()?;
            match frame.msg_type {
                MSG_KEY => return ipc::decode_payload::<RawKeyEvent>(rx.payload(frame.payload_len)).ok(),
                MSG_LAYOUT_CHANGED => {}
                _ => return None,
            }
        }
    }
}

impl AsHandle for Keys {
    fn as_handle(&self) -> crate::BorrowedHandle<'_> {
        self.conn.as_handle()
    }
}

/// Why a surface's host, a terminal, stops serving the shell it started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Leaving {
    /// Its window was closed.
    Closed,
    /// It was asked to quit, and with it everything below it.
    Asked(Reason),
    /// The shell ended.
    ShellEnded,
}

impl Leaving {
    /// What the host asks its shell's tree as it goes: a hang-up for a close
    /// alone. A quit that asked the host reached that tree already, and a
    /// second would kill every process that has not read the first.
    pub fn asks(self) -> Option<Reason> {
        match self {
            Self::Closed => Some(Reason::HangUp),
            Self::Asked(_) | Self::ShellEnded => None,
        }
    }
}

/// The client side of the foreground line: what a shell hands its surface of
/// the line it waits for. A process with no surface above it hands nothing.
#[derive(Default)]
pub struct Foreground {
    conn: Option<Connection>,
}

impl Foreground {
    /// `processes` are the line this process now waits for: a duplicate of
    /// each carrying `MANAGE` is handed over, and the caller keeps its own. The
    /// surface's going is no error: there is nobody left to press Ctrl+C.
    pub fn set(&mut self, processes: &[crate::BorrowedHandle<'_>]) {
        let Some(conn) = self.conn() else { return };
        // `TRANSFER` alone beside `MANAGE`, which the move takes.
        let rights = Rights::MANAGE.union(Rights::TRANSFER);
        let mut copies = processes.iter().take(MAX_LINE).filter_map(|p| p.try_clone_narrowed(rights).ok());
        let mut how = LINE_REPLACE;
        loop {
            let batch: Vec<OwnedHandle> = copies.by_ref().take(MAX_TRANSFER_HANDLES).collect();
            let last = batch.len() < MAX_TRANSFER_HANDLES;
            if conn.send_with(MSG_FOREGROUND, &how, batch).is_err() {
                self.conn = None;
                return;
            }
            if last {
                return;
            }
            how = LINE_ADD;
        }
    }

    /// No line: this process waits for nothing a Ctrl+C should reach.
    pub fn clear(&mut self) {
        self.set(&[]);
    }

    fn conn(&mut self) -> Option<&Connection> {
        if self.conn.is_none() {
            self.conn = crate::endow::service(SERVICE).ok();
        }
        self.conn.as_ref()
    }
}

/// Tell this process's surface that [`LAYOUT_CONFIG`] changed.
///
/// Best effort by construction: the config is already written, so a program
/// with no surface above it has still changed the layout — for the next
/// translator that starts. There is nothing here to report.
pub fn notify_layout_changed() {
    if let Ok(conn) = crate::endow::service(SERVICE) {
        let _: Result<(), ipc::IpcError> = conn.signal(MSG_LAYOUT_CHANGED);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A line takes processes up to its bound, a replacement counting from
    /// none, and refuses the one past it.
    #[test]
    fn a_line_holds_at_most_its_bound() {
        assert!(line_takes(0, MAX_LINE));
        assert!(line_takes(MAX_LINE - MAX_TRANSFER_HANDLES, MAX_TRANSFER_HANDLES));
        assert!(!line_takes(MAX_LINE, 1));
        assert!(!line_takes(MAX_LINE - 1, 2));
    }

    /// Only a closed window asks the shell's tree to hang up. A quit that
    /// asked the terminal reached that tree already, and asking again would
    /// kill what has not read the first; a shell that ended is asked nothing.
    #[test]
    fn a_host_asks_its_shell_to_hang_up_only_when_closed() {
        assert_eq!(Leaving::Closed.asks(), Some(Reason::HangUp));
        for reason in [Reason::Interrupt, Reason::HangUp, Reason::Terminate] {
            assert_eq!(Leaving::Asked(reason).asks(), None, "{reason:?}");
        }
        assert_eq!(Leaving::ShellEnded.asks(), None);
    }

    /// The line set most recently is the foreground, an addition does not move
    /// it, and clearing it hands the foreground back to the line under it.
    #[test]
    fn the_foreground_is_the_line_set_last() {
        let mut lines = Lines::default();
        assert_eq!(lines.foreground(), None);
        lines.set(0, true, true);
        lines.set(2, true, true);
        assert_eq!(lines.foreground(), Some(2), "the nested shell's line");
        lines.set(0, true, false);
        assert_eq!(lines.foreground(), Some(2), "more of an older line");
        lines.set(2, false, true);
        assert_eq!(lines.foreground(), Some(0), "the nested line ended");
        lines.set(1, true, false);
        assert_eq!(lines.foreground(), Some(1), "an addition with no line is a new one");
        lines.set(1, false, false);
        lines.set(0, false, false);
        assert_eq!(lines.foreground(), None);
    }
}
