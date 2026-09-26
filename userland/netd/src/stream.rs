//! One piped TCP stream: the bridge between a smoltcp socket and the two pipes
//! its client holds, and every decision about when the stream ends.
//!
//! **The pipes are the stream's life, not its socket id.** A client that
//! closes the id, exits or is killed leaves netd to finish what the stream owes
//! — the bytes it wrote and then the FIN, or a reset where the peer is still
//! sending to a reader that has gone — and the socket, its slot and its id go
//! only when the stream is over, or when one of these ends it:
//!
//! - [`STALL_LIMIT`]: nothing the stream owes has moved for that long. While
//!   bytes are owed smoltcp probes the peer and ends the stream once the peer
//!   has been silent that long; a FIN owed alone is handed to the same
//!   give-up once it has gone unacknowledged that long; anything an orphan
//!   owes is timed here from the last byte or state the peer acknowledged.
//! - [`FIN_WAIT_2_LIMIT`]: an orphan whose peer acknowledged its FIN and never
//!   sends its own.
//! - [`RST_LINGER`]: a reset stream whose RST cannot leave, because its
//!   neighbour no longer answers.
//!
//! - The closing bound (`NetDaemon::max_closing`): [`PipedConnection::give_up`].
//!
//! A stream whose client is still there is never ended for a peer that keeps
//! its window shut while it answers (RFC 9293 §3.8.6.1: "as long as the
//! receiving TCP peer continues to send acknowledgments in response to the
//! probe segments, the sending TCP peer MUST allow the connection to stay
//! open") — whether what it holds back is bytes or a FIN, which a peer at a
//! zero window may refuse as an unacceptable segment and answer with an ACK.

use std::time::{Duration, Instant};

use smoltcp::iface::SocketHandle;
use smoltcp::socket::tcp;
use toyos::AsHandle;
use toyos_abi::ring::RingHeader;
use toyos_abi::syscall::SyscallError;

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
/// forging it only brings its own stream's bounds forward.
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

/// How long a stream may owe its peer something that does not move before it
/// is reset: RFC 9293 §3.8.3's R2, whose floor is 100 seconds, and this is
/// that floor. Linux gives up after `tcp_retries2` (15), about fifteen
/// minutes; a client with no timeout of its own waits this long on a peer that
/// has gone, and longer only lengthens that wait.
pub const STALL_LIMIT: Duration = Duration::from_secs(100);

/// How often a stream that owes bytes and sends none probes its peer, which is
/// what tells a peer holding its window shut from one that has gone: smoltcp's
/// own ceiling on a retransmission timeout, so a probe is never further apart
/// than a retransmission of the same stream would be.
const PROBE_INTERVAL: Duration = Duration::from_secs(10);

/// How long an orphan whose FIN is acknowledged waits for its peer's: Linux's
/// `tcp_fin_timeout` default, which bounds the same state.
pub const FIN_WAIT_2_LIMIT: Duration = Duration::from_secs(60);

/// How long a reset stream waits for its RST to leave: the time RFC 4861 §10
/// gives a neighbour to answer before the packets waiting on it are dropped
/// (`MAX_MULTICAST_SOLICIT` 3 × `RETRANS_TIMER` 1 s), which Linux's ARP uses
/// too. smoltcp keeps asking for a neighbour that never answers, and keeps the
/// socket until the RST it owes has gone.
pub const RST_LINGER: Duration = Duration::from_secs(3);

fn smoltcp_duration(d: Duration) -> smoltcp::time::Duration {
    smoltcp::time::Duration::from_millis(d.as_millis() as u64)
}

/// How the peer's side of a piped stream ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum End {
    /// Its FIN: every byte it sent is the client's to read, and then EOF.
    Fin,
    /// Anything else — its reset, or netd's own. The client's send pipe is
    /// closed before its receive pipe reaches EOF, and that order is the whole
    /// of what tells the client this EOF is not a FIN: std's pal asks its send
    /// pipe at every EOF.
    Reset,
}

/// What [`PipedConnection::tend`] decided.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fate {
    Open,
    /// Over, and its socket is the caller's to remove.
    Over,
}

/// The last forward progress a stream made: the bytes its peer had
/// acknowledged and the state it was in, and when either last moved.
struct Progress {
    acked: u64,
    state: tcp::State,
    at: Instant,
}

/// A piped TCP stream: data flows through kernel pipes instead of IPC messages.
pub struct PipedConnection<T, F> {
    pub handle: SocketHandle,
    /// The id the client names this stream by, until it closes it.
    pub id: Option<u32>,
    pub rx_write: Option<T>,
    pub tx_read: Option<F>,
    /// The client's receive pipe refused bytes the socket still holds, so the
    /// pipe is watched for room.
    pub held: bool,
    /// The peer's last bytes, moved out of the socket once no more can come:
    /// smoltcp clears a socket's buffer when its TIME-WAIT ends, whether or not
    /// the client has read it.
    tail: Vec<u8>,
    tail_sent: usize,
    end: Option<End>,
    /// The client shut its sending half down: the FIN goes once its send pipe
    /// is empty, which is after every byte it wrote before it asked.
    pub fin_after_drain: bool,
    /// The client's receive end has gone, so a byte the peer sends from here
    /// on is one nobody will read.
    reader_gone: bool,
    /// Bytes of the client's the socket has taken since the stream began.
    taken: u64,
    progress: Progress,
    /// When the client let go of both ends.
    orphaned_at: Option<Instant>,
    /// When the stream was reset, by either side.
    reset_at: Option<Instant>,
    /// Whether the socket probes its peer and gives up on it: while it owes
    /// bytes, and only then, because smoltcp's timeout counts from the last
    /// segment the peer sent and would end an idle stream too.
    probing: bool,
    /// The next moment this stream is owed a pass that no event brings.
    pub wake: Option<Instant>,
}

impl<T: ToClient, F: FromClient> PipedConnection<T, F> {
    pub fn new(handle: SocketHandle, id: u32, to_client: T, from_client: F, now: Instant) -> Self {
        Self {
            handle,
            id: Some(id),
            rx_write: Some(to_client),
            tx_read: Some(from_client),
            held: false,
            tail: Vec::new(),
            tail_sent: 0,
            end: None,
            fin_after_drain: false,
            reader_gone: false,
            taken: 0,
            progress: Progress { acked: 0, state: tcp::State::Established, at: now },
            orphaned_at: None,
            reset_at: None,
            probing: false,
            wake: None,
        }
    }

    /// **A client's handle that refuses netd for any reason but a full pipe or
    /// a vanished reader ends that client's stream, never netd.** The ends are
    /// whatever the client moved, and nothing checks their kind at intake: a
    /// read end, a file past its size limit or a handle with no `WRITE` right
    /// each answer a refusal here. So does a pipe whose ring page could not be
    /// allocated, which no wait cures.
    fn refuse(&mut self, socket: &mut tcp::Socket, end: &str, e: SyscallError, now: Instant) {
        say!("netd: resetting a connection — its {end} pipe refused netd: {e:?}");
        self.reset(socket, now);
        self.close_rx();
    }

    /// End the stream with a reset, and close the client's send pipe at once:
    /// its next write fails, and the EOF its receive pipe reaches after the
    /// bytes still owed reads as a reset.
    fn reset(&mut self, socket: &mut tcp::Socket, now: Instant) {
        socket.abort();
        self.end = Some(End::Reset);
        self.reset_at = Some(now);
        self.close_tx();
    }

    fn close_rx(&mut self) {
        self.rx_write.take();
    }

    fn close_tx(&mut self) {
        self.tx_read.take();
    }

    /// Whether netd holds neither of the client's pipes: the stream costs a
    /// socket and nothing of the client's.
    pub fn client_done(&self) -> bool {
        self.rx_write.is_none() && self.tx_read.is_none()
    }

    /// Whether the client has let go of both ends, though its send pipe may
    /// still hold bytes netd has yet to send: the stream is an orphan.
    fn client_gone(&self) -> bool {
        self.rx_write.is_none() && self.tx_read.as_ref().is_none_or(FromClient::writer_gone)
    }

    /// Whether this stream still owes its peer bytes or a FIN.
    pub fn owes(&self, socket: &tcp::Socket) -> bool {
        socket.send_queue() > 0
            || matches!(socket.state(), tcp::State::FinWait1 | tcp::State::Closing | tcp::State::LastAck)
    }

    /// When the client let go of both ends, if it has.
    pub fn orphaned_at(&self) -> Option<Instant> {
        self.orphaned_at
    }

    /// Whether the client has let go of both pipes and the stream is still
    /// finishing, not reset: what the closing bound counts. A reset one is
    /// gone within [`RST_LINGER`].
    pub fn closing(&self) -> bool {
        self.client_done() && self.reset_at.is_none()
    }

    /// Give this closing stream up, for the closing bound: one that still owes
    /// its peer bytes or its FIN is reset and lingers as any reset does, so a
    /// peer waiting on it is told; one that owes nothing is over at once, and a
    /// peer that sends to it again is answered smoltcp's RST. Linux resets the
    /// orphan it cannot keep, and keeps no TIME-WAIT past its bucket limit.
    pub fn give_up(&mut self, socket: &mut tcp::Socket, now: Instant) -> Fate {
        if !self.owes(socket) {
            return Fate::Over;
        }
        self.reset(socket, now);
        self.wake = Some(now + RST_LINGER);
        Fate::Open
    }

    fn tail_left(&self) -> &[u8] {
        &self.tail[self.tail_sent..]
    }

    /// The peer's bytes, socket to the client's receive pipe, and the end of
    /// them once they are all there.
    ///
    /// **Nothing leaves the socket that the pipe did not take**: a byte
    /// dequeued here has already been acknowledged to the peer, so one the
    /// pipe refused is cut out of the middle of the client's stream with
    /// nothing saying so. The rest waits in the socket, and the pipe's room is
    /// what wakes the pass that moves it.
    pub fn receive(&mut self, socket: &mut tcp::Socket, now: Instant) {
        if self.end.is_none() && !receiving(socket.state()) {
            while socket.can_recv() {
                socket
                    .recv(|queued| {
                        self.tail.extend_from_slice(queued);
                        (queued.len(), ())
                    })
                    .expect("netd: a socket holding bytes refused to give them up");
            }
            self.end = Some(match socket.recv(|_| (0, ())) {
                Err(tcp::RecvError::Finished) => End::Fin,
                Err(tcp::RecvError::InvalidState) => End::Reset,
                Ok(()) => unreachable!("netd: a {} socket with no bytes still receives", socket.state()),
            });
            if self.end == Some(End::Reset) {
                self.reset_at = Some(now);
                self.close_tx();
            }
        }

        self.held = false;
        if let Some(pipe) = self.rx_write.take() {
            let mut refused = None;
            while !self.tail_left().is_empty() {
                match pipe.write(self.tail_left()) {
                    Ok(0) => break,
                    Ok(n) => self.tail_sent += n,
                    Err(e) => {
                        refused = Some(e);
                        break;
                    }
                }
            }
            while refused.is_none() && self.tail.is_empty() && socket.can_recv() {
                let moved = socket.recv(|queued| match pipe.write(queued) {
                    Ok(n) => (n, n),
                    Err(e) => {
                        refused = Some(e);
                        (0, 0)
                    }
                });
                if !matches!(moved, Ok(n) if n > 0) {
                    break;
                }
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
                Some(SyscallError::WouldBlock) => self.held = !self.tail_left().is_empty() || socket.can_recv(),
                Some(SyscallError::Gone) => {
                    self.reader_gone = true;
                    self.close_rx();
                }
                Some(e) => self.refuse(socket, "receive", e, now),
            }
        }
        if self.tail_left().is_empty() && !self.tail.is_empty() {
            self.tail = Vec::new();
            self.tail_sent = 0;
        }
        // Every byte of the peer's is in the pipe: the end follows them.
        if self.end.is_some() && self.tail.is_empty() && !socket.can_recv() {
            self.close_rx();
        }
        // A reader that has gone with the peer still sending: the peer is
        // told at once, as a close with unread data tells it (RFC 2525 §2.17).
        if self.reader_gone && socket.is_open() && (socket.can_recv() || !self.tail.is_empty()) {
            self.reset(socket, now);
            self.tail = Vec::new();
        }
    }

    /// The client's bytes, its send pipe to the socket, and the FIN after
    /// them once the client closes its end or shuts its sending half down.
    ///
    /// Ok(0) is the kernel's EOF — ring drained, no writer — which says the
    /// client stopped writing; not the forgeable closed flags. [`send_room`]
    /// and never `can_send` alone: a zero-length read answers `Ok(0)`, which
    /// would read as the client hanging up.
    pub fn send(&mut self, socket: &mut tcp::Socket, now: Instant) {
        let Some(pipe) = self.tx_read.take() else { return };
        if !matches!(socket.state(), tcp::State::Established | tcp::State::CloseWait) {
            // The socket sends nothing more — its FIN is queued, or it is
            // over. What the pipe says now is only whether the client has
            // gone, or wrote bytes that can never be sent, which it is told
            // by the pipe's closing: its next write fails.
            let mut byte = [0u8; 1];
            match pipe.read(&mut byte) {
                Ok(_) => drop(pipe),
                Err(SyscallError::WouldBlock) => self.tx_read = Some(pipe),
                Err(e) => {
                    // Closed before the receive pipe, as a reset's order is.
                    drop(pipe);
                    self.refuse(socket, "send", e, now);
                }
            }
            return;
        }
        // **No more is taken out of the pipe than the socket will take**: the
        // read lands in the send buffer's own free space, so a byte the pipe
        // gave up is a byte the socket holds.
        while send_room(socket) {
            let read = socket
                .send(|room| match pipe.read(room) {
                    Ok(n) => (n, Ok(n)),
                    Err(e) => (0, Err(e)),
                })
                .unwrap_or_else(|e| panic!("netd: a socket that could send refused: {e:?}"));
            match read {
                Ok(0) => {
                    socket.close();
                    return;
                }
                Ok(n) => self.taken += n as u64,
                Err(SyscallError::WouldBlock) => {
                    if self.fin_after_drain {
                        socket.close();
                    }
                    self.tx_read = Some(pipe);
                    return;
                }
                Err(e) => {
                    self.tx_read = Some(pipe);
                    self.refuse(socket, "send", e, now);
                    return;
                }
            }
        }
        self.tx_read = Some(pipe);
    }

    /// Decide, after a pass has moved what it could, whether this stream is
    /// over, is to be reset, or waits — and until when.
    pub fn tend(&mut self, socket: &mut tcp::Socket, now: Instant) -> Fate {
        let acked = self.taken - socket.send_queue() as u64;
        if acked != self.progress.acked || socket.state() != self.progress.state {
            self.progress = Progress { acked, state: socket.state(), at: now };
        }
        self.probe(socket, now);
        if self.client_gone() {
            self.orphaned_at.get_or_insert(now);
        }
        self.wake = None;
        if let Some(reset_at) = self.reset_at {
            // Its client still reads what the peer sent before the reset.
            if !self.client_done() {
                return Fate::Open;
            }
            let given_up = reset_at + RST_LINGER;
            if finished(socket) || now >= given_up {
                return Fate::Over;
            }
            self.wake = Some(given_up);
            return Fate::Open;
        }
        if self.client_done() && finished(socket) {
            return Fate::Over;
        }
        if let Some((limit, why)) = self.limit(socket) {
            if now < limit {
                self.wake = Some(limit);
            } else {
                say!("netd: resetting a connection — {why}");
                self.reset(socket, now);
                self.wake = Some(now + RST_LINGER);
                return Fate::Open;
            }
        }
        if !self.probing && fin_alone(socket) {
            let arm = self.progress.at + STALL_LIMIT;
            self.wake = Some(self.wake.map_or(arm, |w| w.min(arm)));
        }
        Fate::Open
    }

    /// Arm smoltcp's probes and its give-up while the socket owes bytes, or a
    /// FIN alone that has gone unacknowledged for [`STALL_LIMIT`], and disarm
    /// them when it does not. Armed before the pass that sends what the
    /// client just wrote: smoltcp starts the timeout's clock at the first
    /// segment after its buffer was empty.
    ///
    /// **A FIN owed alone is armed late, not at once**: nothing restarts
    /// smoltcp's clock for a FIN, which it counts from the peer's last
    /// segment, and on a stream idle before its close that is older than the
    /// limit — armed then, the FIN would be aborted unsent.
    fn probe(&mut self, socket: &mut tcp::Socket, now: Instant) {
        let owed = socket.send_queue() > 0 || (fin_alone(socket) && now >= self.progress.at + STALL_LIMIT);
        if owed != self.probing {
            socket.set_timeout(owed.then(|| smoltcp_duration(STALL_LIMIT)));
            socket.set_keep_alive(owed.then(|| smoltcp_duration(PROBE_INTERVAL)));
            self.probing = owed;
        }
    }

    /// The moment this stream is reset unless it moves first, and why.
    fn limit(&self, socket: &tcp::Socket) -> Option<(Instant, &'static str)> {
        match self.orphaned_at {
            Some(at) if socket.state() == tcp::State::FinWait2 => {
                Some((at.max(self.progress.at) + FIN_WAIT_2_LIMIT, "an orphan's peer never sent its FIN"))
            }
            Some(at) if self.owes(socket) => {
                Some((at.max(self.progress.at) + STALL_LIMIT, "an orphan's peer acknowledged nothing for its limit"))
            }
            _ => None,
        }
    }
}

/// Whether `socket` owes its peer its FIN and nothing else.
fn fin_alone(socket: &tcp::Socket) -> bool {
    matches!(socket.state(), tcp::State::FinWait1 | tcp::State::Closing | tcp::State::LastAck) && socket.send_queue() == 0
}

/// Whether a socket in `state` can still be sent bytes by its peer.
fn receiving(state: tcp::State) -> bool {
    matches!(state, tcp::State::Established | tcp::State::FinWait1 | tcp::State::FinWait2)
}

/// Whether a socket is over and has sent everything it owes: closed, and
/// forgotten by smoltcp only once its last RST or ACK has gone.
fn finished(socket: &tcp::Socket) -> bool {
    socket.state() == tcp::State::Closed && socket.remote_endpoint().is_none()
}

/// Whether `socket` takes a client's bytes now: it is sending, and its send
/// buffer has room.
pub fn send_room(socket: &tcp::Socket) -> bool {
    socket.can_send() && socket.send_capacity() > socket.send_queue()
}

/// Which closing streams a closing table holding more than `bound` gives up:
/// the newest, as Linux gives up the stream being orphaned rather than one it
/// already keeps. `closing` is each one's index and when its client let go.
pub fn past_bound(closing: impl Iterator<Item = (usize, Instant)>, bound: usize) -> Vec<usize> {
    let mut closing: Vec<_> = closing.collect();
    closing.sort_unstable_by_key(|&(_, orphaned_at)| std::cmp::Reverse(orphaned_at));
    let excess = closing.len().saturating_sub(bound);
    closing[..excess].iter().map(|&(i, _)| i).collect()
}

#[cfg(test)]
mod tests;
