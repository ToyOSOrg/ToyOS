//! One piped TCP stream: the bridge between a connection of the stack's and
//! the two pipes its client holds, and every decision netd makes about when
//! the stream ends.
//!
//! **The pipes are the stream's life, not its socket id.** A client that
//! closes the id, exits or is killed leaves netd to move what it wrote into
//! the connection and ask for the FIN after it; once the client holds neither
//! pipe the connection is the stack's own ([`PipedConnection::finish`]), which
//! finishes it as any stack finishes a closed socket — its retransmission
//! limit for a peer that has gone, its FIN-WAIT-2 timeout, its TIME-WAIT — and
//! netd keeps only its cookie, for its closing bound ([`Closing`]).
//!
//! What stays netd's is the client's side: the order the client learns the
//! peer's end in ([`End`]), a reader that leaves with the peer still sending
//! (reset at once, RFC 2525 §2.17), a pipe that refuses netd (reset), and how
//! many closed connections netd lets the stack keep for its clients
//! ([`past_bound`]).

use std::sync::Arc;
use std::time::Instant;

use net_types::ip::Ipv4;
use netstack3_core::device::WeakDeviceId;
use netstack3_core::socket::{IpSocketMatcher, SocketCookieMatcher};
use netstack3_core::socket::ShutdownType;
use netstack3_core::tcp::{TcpSocketId, TcpSocketState};
use toyos::AsHandle;
use toyos_abi::ring::RingHeader;
use toyos_abi::syscall::SyscallError;

use crate::net::Net;
use crate::stack::{Bindings, Shared};

/// A TCP socket of the stack's.
pub type TcpId = TcpSocketId<Ipv4, WeakDeviceId<Bindings>, Bindings>;

/// netd's end of the pipe it writes the peer's bytes into.
pub trait ToClient {
    /// A write that never waits.
    fn write(&self, bytes: &[u8]) -> Result<usize, SyscallError>;
}

/// netd's end of the pipe it reads the client's bytes from.
pub trait FromClient {
    /// A read that never waits.
    fn read(&self, buf: &mut [u8]) -> Result<usize, SyscallError>;
    /// Whether the client has let go of its end, whatever the pipe still
    /// holds: a read says so only once every byte is out.
    fn writer_gone(&self) -> bool;
}

impl ToClient for toyos::Pipe {
    fn write(&self, bytes: &[u8]) -> Result<usize, SyscallError> {
        self.write_nonblock(bytes)
    }
}

/// The client's send pipe, with its ring's header mapped: the kernel marks
/// there that the pipe's writer has gone, which is how netd learns a client
/// left bytes behind it.
///
/// **The mark is the client's to forge**, since any holder may map the page;
/// forging it only makes its own stream an orphan early.
pub struct SendPipe {
    pipe: toyos::Pipe,
    header: *const RingHeader,
}

impl SendPipe {
    /// Map `pipe`'s header, or `None` where the kernel refuses: a handle that
    /// is not a pipe, or one moved without the right to map it.
    pub fn map(pipe: toyos::Pipe) -> Option<Self> {
        let header = pipe.pipe_map().ok()? as *const RingHeader;
        Some(Self { pipe, header })
    }
}

impl FromClient for SendPipe {
    fn read(&self, buf: &mut [u8]) -> Result<usize, SyscallError> {
        self.pipe.read_nonblock(buf)
    }

    fn writer_gone(&self) -> bool {
        // SAFETY: the window `map` made lives as long as `pipe`, which this
        // owns; the header is one atomic, read as one.
        unsafe { &*self.header }.is_writer_closed()
    }
}

impl AsHandle for SendPipe {
    fn as_handle(&self) -> toyos_abi::RawHandle {
        self.pipe.as_handle()
    }
}

/// How the peer's side of a piped stream ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum End {
    /// Its FIN: every byte it sent is the client's to read, and then EOF.
    Fin,
    /// Anything else — its reset, the stack giving up on it, or netd's own
    /// reset. The client's send pipe is closed before its receive pipe reaches
    /// EOF, and that order is the whole of what tells the client this EOF is
    /// not a FIN: std's pal asks its send pipe at every EOF.
    Reset,
}

/// The state the stack holds a connection in.
pub fn state(net: &mut Net, id: &TcpId) -> TcpSocketState {
    net.api().tcp::<Ipv4>().get_tcp_info(id).state
}

/// Whether a connection in `state` still takes bytes to send.
fn sending(state: TcpSocketState) -> bool {
    matches!(state, TcpSocketState::Established | TcpSocketState::CloseWait)
}

/// A piped TCP stream: data flows through kernel pipes instead of IPC messages.
pub struct PipedConnection<T, F> {
    pub id: TcpId,
    shared: Arc<Shared>,
    /// The id the client names this stream by, until it closes it.
    pub socket_id: Option<u32>,
    pub rx_write: Option<T>,
    pub tx_read: Option<F>,
    /// The client's receive pipe refused bytes the connection still holds, so
    /// the pipe is watched for room.
    pub held: bool,
    end: Option<End>,
    /// The client shut its sending half down: the FIN goes once its send pipe
    /// is empty, which is after every byte it wrote before it asked.
    pub fin_after_drain: bool,
    /// The FIN has been asked of the stack.
    fin_asked: bool,
    /// The client's receive end has gone, so a byte the peer sends from here
    /// on is one nobody will read.
    reader_gone: bool,
    /// netd reset the connection itself.
    reset: bool,
    /// When the client let go of both ends, its send pipe maybe not yet
    /// drained: the stream is an orphan.
    orphaned_at: Option<Instant>,
}

impl<T: ToClient, F: FromClient> PipedConnection<T, F> {
    pub fn new(id: TcpId, shared: Arc<Shared>, socket_id: u32, to_client: T, from_client: F) -> Self {
        Self {
            id,
            shared,
            socket_id: Some(socket_id),
            rx_write: Some(to_client),
            tx_read: Some(from_client),
            held: false,
            end: None,
            fin_after_drain: false,
            fin_asked: false,
            reader_gone: false,
            reset: false,
            orphaned_at: None,
        }
    }

    /// **A client's handle that refuses netd for any reason but a full pipe or
    /// a vanished reader ends that client's stream, never netd.** The ends are
    /// whatever the client moved, and nothing checks their kind at intake: a
    /// read end, a file past its size limit or a handle with no `WRITE` right
    /// each answer a refusal here.
    fn refuse(&mut self, net: &mut Net, end: &str, e: SyscallError) {
        say!("netd: resetting a connection — its {end} pipe refused netd: {e:?}");
        self.reset(net);
        self.rx_write.take();
    }

    /// End the stream with a reset, and close the client's send pipe at once:
    /// its next write fails, and the EOF its receive pipe reaches after the
    /// bytes still owed reads as a reset.
    pub fn reset(&mut self, net: &mut Net) {
        if !self.reset {
            abort(net, &self.id);
            self.reset = true;
        }
        self.end = Some(End::Reset);
        self.tx_read.take();
    }

    /// Whether netd holds neither of the client's pipes: the connection is the
    /// stack's to finish.
    pub fn client_done(&self) -> bool {
        self.rx_write.is_none() && self.tx_read.is_none()
    }

    /// When the client let go of both ends, if it has.
    pub fn orphaned_at(&self) -> Option<Instant> {
        self.orphaned_at
    }

    /// The peer's bytes, the shared ring to the client's receive pipe, and the
    /// end of them once they are all there.
    ///
    /// **Nothing leaves the ring that the pipe did not take**: a byte there
    /// has already been acknowledged to the peer, so one the pipe refused is
    /// cut out of the middle of the client's stream with nothing saying so.
    /// The rest waits, the window shuts, and the pipe's room is what wakes the
    /// pass that moves it.
    pub fn receive(&mut self, net: &mut Net) {
        if self.end.is_none() && !self.shared.receiving() {
            // The stack lets go of the receive ring at the peer's FIN, and at
            // a reset or its own giving up, whose state is closed. No close of
            // netd's can finish a connection before a pass has seen its FIN,
            // so closed here is never one.
            self.end = Some(match state(net, &self.id) {
                TcpSocketState::Close => End::Reset,
                _ => End::Fin,
            });
            if self.end == Some(End::Reset) {
                self.tx_read.take();
            }
        }
        self.held = false;
        if let Some(pipe) = self.rx_write.take() {
            let mut refused = None;
            let mut moved = 0;
            loop {
                let took = self.shared.read(|bytes| match pipe.write(bytes) {
                    Ok(n) => n,
                    Err(e) => {
                        refused = Some(e);
                        0
                    }
                });
                if took == 0 {
                    break;
                }
                moved += took;
            }
            if moved > 0 && self.end.is_none() {
                net.api().tcp::<Ipv4>().on_receive_buffer_read(&self.id);
            }
            // Detect the client leaving: a zero-byte write is refused by name
            // once the pipe has no reader — the kernel's fact, not the
            // client's.
            if refused.is_none() {
                refused = pipe.write(&[]).err();
            }
            self.rx_write = Some(pipe);
            match refused {
                None => {}
                // Full: the client has not read yet.
                Some(SyscallError::WouldBlock) => self.held = self.shared.unread() > 0,
                Some(SyscallError::Gone) => {
                    self.reader_gone = true;
                    self.rx_write.take();
                }
                Some(e) => self.refuse(net, "receive", e),
            }
        }
        // Every byte of the peer's is in the pipe: the end follows them.
        if self.end.is_some() && self.shared.unread() == 0 {
            self.rx_write.take();
        }
        // A reader that has gone with the peer still sending: the peer is
        // told at once, as a close with unread data tells it (RFC 2525 §2.17).
        if self.reader_gone && self.end.is_none() && self.shared.unread() > 0 {
            self.reset(net);
        }
    }

    /// The client's bytes, its send pipe to the connection's send ring, and
    /// the FIN after them once the client closes its end or shuts its sending
    /// half down.
    ///
    /// Ok(0) is the kernel's EOF — ring drained, no writer — which says the
    /// client stopped writing; not the forgeable closed flags. A read is never
    /// asked with no room: a zero-length read answers `Ok(0)` too.
    pub fn send(&mut self, net: &mut Net) {
        let Some(pipe) = self.tx_read.take() else { return };
        if self.fin_asked || !sending(state(net, &self.id)) {
            // The connection sends nothing more — its FIN is asked for, or it
            // is over. What the pipe says now is only whether the client has
            // gone, or wrote bytes that can never be sent, which it is told by
            // the pipe's closing: its next write fails.
            let mut byte = [0u8; 1];
            match pipe.read(&mut byte) {
                Ok(_) => drop(pipe),
                Err(SyscallError::WouldBlock) => self.tx_read = Some(pipe),
                Err(e) => {
                    drop(pipe);
                    self.refuse(net, "send", e);
                }
            }
            return;
        }
        // **No more is taken out of the pipe than the ring will take**: the
        // read lands in a buffer no larger than the send ring's free space, so
        // a byte the pipe gave up is a byte the connection holds.
        let mut buf = [0u8; 16 * 1024];
        let mut queued = false;
        let outcome = loop {
            let room = net
                .api()
                .tcp::<Ipv4>()
                .with_send_buffer(&self.id, |ring| ring.room())
                .expect("netd: a sending connection has a send ring");
            if room == 0 {
                break Read::Full;
            }
            let want = room.min(buf.len());
            match pipe.read(&mut buf[..want]) {
                Ok(0) => break Read::Eof,
                Ok(n) => {
                    let took = net
                        .api()
                        .tcp::<Ipv4>()
                        .with_send_buffer(&self.id, |ring| ring.push(&buf[..n]))
                        .expect("netd: a sending connection has a send ring");
                    assert_eq!(took, n, "netd: a send ring refused bytes it had room for");
                    queued = true;
                }
                Err(SyscallError::WouldBlock) => break Read::Empty,
                Err(e) => break Read::Refused(e),
            }
        };
        if queued {
            net.api().tcp::<Ipv4>().do_send(&self.id);
        }
        match outcome {
            // The ring is full; the ACK that makes room wakes the NIC.
            Read::Full => self.tx_read = Some(pipe),
            Read::Eof => self.ask_fin(net),
            Read::Empty => {
                if self.fin_after_drain {
                    self.ask_fin(net);
                }
                self.tx_read = Some(pipe);
            }
            Read::Refused(e) => {
                self.tx_read = Some(pipe);
                self.refuse(net, "send", e);
            }
        }
    }

    fn ask_fin(&mut self, net: &mut Net) {
        if !self.fin_asked {
            // Refused only by a connection already over, whose end the
            // receive side reads.
            let _ = net.api().tcp::<Ipv4>().shutdown(&self.id, ShutdownType::Send);
            self.fin_asked = true;
        }
    }

    /// Note the moment the client let go of both ends.
    pub fn tend(&mut self, now: Instant) {
        let gone = self.rx_write.is_none() && self.tx_read.as_ref().is_none_or(FromClient::writer_gone);
        if gone {
            self.orphaned_at.get_or_insert(now);
        }
    }

    /// Whether the connection takes the client's bytes now: it sends and its
    /// ring has room, which is when the send pipe is worth watching.
    pub fn send_room(&self, net: &mut Net) -> bool {
        !self.fin_asked
            && sending(state(net, &self.id))
            && net.api().tcp::<Ipv4>().with_send_buffer(&self.id, |ring| ring.room() > 0) == Some(true)
    }

    /// Hand a connection the client is done with to the stack, which finishes
    /// what it owes alone; netd keeps its cookie for its closing bound, and
    /// nothing of one it reset, which is over.
    pub fn finish(self, net: &mut Net, now: Instant) -> Option<Closing> {
        assert!(self.client_done(), "netd: finished a stream whose client still holds a pipe");
        let cookie = self.id.socket_cookie().export_value();
        let since = self.orphaned_at.unwrap_or(now);
        let reset = self.reset;
        net.api().tcp::<Ipv4>().close(self.id);
        (!reset).then_some(Closing { cookie, since })
    }
}

/// What a read of the client's send pipe came to.
enum Read {
    /// The send ring has no room left.
    Full,
    /// The client stopped writing.
    Eof,
    /// Nothing more is in the pipe yet.
    Empty,
    Refused(SyscallError),
}

/// Reset `id`: the stack sends the RST its state owes, and lets go.
pub fn abort(net: &mut Net, id: &TcpId) {
    reset_cookie(net, id.socket_cookie().export_value());
}

fn reset_cookie(net: &mut Net, cookie: u64) {
    let matcher = IpSocketMatcher::Cookie(SocketCookieMatcher { cookie, invert: false });
    let _ = net.api().tcp::<Ipv4>().disconnect_bound(&matcher);
}

/// A connection its client let go of, which the stack is finishing, named by
/// its cookie: netd keeps no reference to it.
pub struct Closing {
    pub cookie: u64,
    /// When its client let go.
    pub since: Instant,
}

impl Closing {
    /// Give it up now: reset where it still owes its peer anything, and gone
    /// at once either way. Linux resets the orphan it cannot keep, and keeps
    /// no TIME-WAIT past its bucket limit.
    pub fn give_up(&self, net: &mut Net) {
        reset_cookie(net, self.cookie);
    }
}

/// Every TCP socket the stack holds bound, by cookie, and the state it is in:
/// one sweep of the stack's sockets.
pub fn census(net: &mut Net) -> std::collections::HashMap<u64, TcpSocketState> {
    let mut found = Vec::new();
    let every: &[IpSocketMatcher<()>] = &[];
    net.api().tcp::<Ipv4>().bound_sockets_diagnostics(every, &mut found, false);
    found.into_iter().map(|d| (d.cookie.export_value(), d.state_machine)).collect()
}

/// Which closing connections a closing table holding more than `bound` gives
/// up: the newest, as Linux gives up the socket being orphaned rather than one
/// it already keeps. `closing` is each one's index and when its client let go.
pub fn past_bound(closing: impl Iterator<Item = (usize, Instant)>, bound: usize) -> Vec<usize> {
    let mut closing: Vec<_> = closing.collect();
    closing.sort_unstable_by_key(|&(_, since)| std::cmp::Reverse(since));
    let excess = closing.len().saturating_sub(bound);
    closing[..excess].iter().map(|&(i, _)| i).collect()
}

