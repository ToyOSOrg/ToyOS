//! Streams: a connection of [tcp]'s whose two buffers are the region its client lent
//! (`toyos::net::stream`), and the session's server end over the region's words. [`Session`]
//! keeps the mapping and the doorbells in netstack, so tests lend a region on the heap.
//!
//! **A pass** ([`Node::bridge`]) reads the client's head into [tcp]'s window (RFC 9293
//! §3.8.6.2.2), publishes what [tcp] holds in order and the peer's FIN after it, gives
//! acknowledged bytes back as room, queues what the client published with no copy, and turns the
//! client's shutdown into the FIN after its last byte. A failure is stored as the receive ring's
//! `end`, which wakes the client either way. netstack arms `tx_data` while it took everything and
//! [tcp] has room, and `rx_room` only while the window last offered is below the update
//! threshold.
//!
//! **Every word read from the region is the client's, and judged**: a cursor past its bound, or
//! text published after its own shutdown, is a [`Violation`] that resets that stream alone.
//!
//! **A stream has holders**: the client that connected or accepted it, and one more for each
//! [`Node::export`], each a process of its own over the one region, each taking a place past
//! the first. A holder that leaves while others stay ([`Node::departed`]) has the gates it held
//! let go and the rings it may have skipped rung, and a pass takes up every cursor it moved, so
//! no holder that stays waits on it.
//!
//! **A stream ends with its connection or its last holder.** Ended, the stream is let go and
//! the holders read what the region holds. The last holder's leaving closes the connection,
//! which [tcp] finishes alone: what was published is sent before the FIN (§3.10.4), or a reset
//! is sent where text is unread (§3.6.1).

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use core::sync::atomic::Ordering;
use core::time::Duration;

use toyos::net::stream::{Server, Words, ADDRESS_GONE, LAYOUT, NODELAY, RECEIVE_BYTES, RESET, TIMED_OUT, UNREACHABLE};
use toyos_net_shard::ConnectError;
use toyos_net_tcp::{ConnId, Endpoint, Error, Failure, Lent, Options, State, Status};
use toyos_net_wire::Instant;
use toyos::transport::session::{Holder, Rings};
use toyos::transport::Violation;

use crate::lease::Stack;
use crate::Node;

/// A stream's session: the region its client lent and every holder's two doorbells. Dropped,
/// the node holds none of them, and each holder's next look finds the `end` or the hang-up.
pub trait Session {
    /// The region's header.
    fn words(&self) -> &Words;
    /// The region's receive ring, as [tcp]'s receive buffer.
    fn receive(&self) -> Box<dyn Lent>;
    /// The region's send ring, as [tcp]'s send buffer.
    fn send(&self) -> Box<dyn Lent>;
    /// Rings the doorbells `rings` names, of every holder, never waiting.
    fn ring(&self, rings: Rings);
}

/// Why a connect made no stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectRefused {
    /// The node holds all it has places for (`places`).
    Full,
    Stack(ConnectError),
}

/// Why an export made no holder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportRefused {
    /// The node holds all it has places for, and a holder past a socket's first takes one.
    Full,
    /// The id names no established stream, listener or datagram socket the node holds.
    Gone,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StreamId(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamEvent {
    /// The answer to a connect: established, from this address and port.
    Connected { id: StreamId, local: Endpoint },
    /// The answer to a connect: the peer refused it or [tcp] gave it up.
    Failed { id: StreamId, failure: Failure },
    /// The answer to a connect: its deadline passed first.
    TimedOut { id: StreamId },
    /// The answer to a connect: its client left first.
    Closed { id: StreamId },
    /// A line for the log: the client wrote what no client writes, and its stream was reset.
    Violated { id: StreamId, violation: Violation },
}

/// The receive ring's `end` for a failure.
const fn code(failure: Failure) -> u32 {
    match failure {
        Failure::TimedOut => TIMED_OUT,
        Failure::Unreachable(_) | Failure::Prohibited => UNREACHABLE,
        Failure::Refused | Failure::Reset => RESET,
        Failure::AddressGone => ADDRESS_GONE,
    }
}

/// Whether the peer's FIN came after every byte before it.
fn fin_received(status: &Status) -> bool {
    match status.state {
        State::CloseWait | State::Closing | State::LastAck | State::TimeWait => true,
        State::Closed => status.failure.is_none(),
        State::SynSent | State::SynReceived | State::Established | State::FinWait1 | State::FinWait2 => false,
    }
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

struct Stream {
    conn: ConnId,
    session: Box<dyn Session>,
    /// The session's server end, once the connection is established and its buffers are the
    /// region's; `None` while a connect is not answered.
    server: Option<Server>,
    /// The connect's, while connecting.
    deadline: Option<Instant>,
    /// Bytes published that the client had not released, as the last pass read them.
    held: u32,
    /// Bytes of the send ring [tcp] holds, as the last pass counted them.
    queued: u32,
    /// The client's FIN is queued after its last byte.
    fin_queued: bool,
    /// The options last written to [tcp].
    options: Options,
    /// A holder rang since the last pass.
    rung: bool,
    /// Its holders past the first, each of which takes a place.
    more_holders: usize,
}

impl Stream {
    /// A connection whose buffers are the region's from here on, and its server end.
    fn lent(&mut self, stack: &mut Stack) {
        let session = &self.session;
        if stack.tcp_lend(self.conn, session.receive(), session.send()).is_err() {
            unreachable!("[tcp] would not lend an established connection its client's buffers");
        }
        self.server = Some(Server::new(session.words(), LAYOUT));
    }

    /// One pass. `false` lets the stream go: its connection is [tcp]'s alone or gone.
    fn pass(&mut self, id: StreamId, now: Instant, stack: &mut Stack, events: &mut VecDeque<StreamEvent>) -> bool {
        let conn = self.conn;
        if self.server.is_none() {
            let status = stack.tcp_status(conn);
            let event = match (status.state, status.failure) {
                (_, Some(failure)) => StreamEvent::Failed { id, failure },
                (State::SynSent | State::SynReceived, None) if self.deadline.is_some_and(|at| at <= now) => StreamEvent::TimedOut { id },
                (State::SynSent | State::SynReceived, None) => return true,
                (_, None) => StreamEvent::Connected { id, local: stack.tcp_local(conn) },
            };
            events.push_back(event);
            if !matches!(event, StreamEvent::Connected { .. }) {
                stack.tcp_abort(now, conn);
                return false;
            }
            self.deadline = None;
            self.lent(stack);
        }
        match self.moved(now, stack) {
            Ok(true) => true,
            Ok(false) => {
                stack.tcp_close(now, conn);
                false
            }
            Err(violation) => {
                if let Some(server) = self.server.as_mut() {
                    self.session.ring(server.fail(self.session.words(), RESET));
                }
                stack.tcp_abort(now, conn);
                events.push_back(StreamEvent::Violated { id, violation });
                false
            }
        }
    }

    /// Moves what changed, round after round until a look after netstack's arms finds nothing
    /// more, and rings the client once: `false` once the connection ended.
    fn moved(&mut self, now: Instant, stack: &mut Stack) -> Result<bool, Violation> {
        let conn = self.conn;
        let Some(server) = self.server.as_mut() else { unreachable!("a stream moves only once established") };
        let words = self.session.words();
        let mut rings = Rings::default();
        let keep = loop {
            let held = server.held(words)?;
            let released = self.held.checked_sub(held).ok_or(Violation::HeadPastTail)?;
            if released > 0 {
                stack.tcp_consume(now, conn, released);
            }
            let status = stack.tcp_status(conn);
            let unread = count(status.readable);
            if let Some(new) = unread.checked_sub(held).filter(|&new| new > 0) {
                rings = rings.and(server.publish(words, new)?);
            }
            self.held = unread;
            if fin_received(&status) {
                rings = rings.and(server.finish(words));
            }

            let queued = count(status.queued);
            if let Some(acked) = self.queued.checked_sub(queued).filter(|&acked| acked > 0) {
                rings = rings.and(server.release(words, acked)?);
            }
            self.queued = queued;
            let shut = server.shut(words);
            let fresh = server.sent(words)?.checked_sub(queued).ok_or(Violation::TailPastDepth)?;
            if fresh > 0 && status.failure.is_none() {
                match stack.tcp_queue(now, conn, fresh) {
                    // [tcp]'s send buffer is the whole ring, so what the ring held fits it.
                    Ok(taken) if count(taken) == fresh => self.queued = self.queued.saturating_add(fresh),
                    // Text published past the client's own shutdown.
                    Err(Error::Closing) => return Err(Violation::TailPastDepth),
                    Err(Error::Failed(_)) => {}
                    answer => unreachable!("[tcp] queued {answer:?} of {fresh} bytes its client's ring held"),
                }
            }
            if shut && !self.fin_queued && status.failure.is_none() {
                stack.tcp_shutdown_write(now, conn);
                self.fin_queued = true;
            }
            let nodelay = words[NODELAY].load(Ordering::Relaxed) != 0;
            if nodelay != self.options.nodelay && status.failure.is_none() && status.state != State::Closed {
                self.options.nodelay = nodelay;
                stack.tcp_set_options(now, conn, self.options);
            }

            if let Some(failure) = status.failure {
                rings = rings.and(server.fail(words, code(failure)));
                break false;
            }
            if matches!(status.state, State::Closed | State::TimeWait) {
                break false;
            }
            if core::mem::take(&mut self.rung) {
                rings = rings.and(server.rung(words)?);
            }
            let mut more = false;
            if !shut && status.writable > 0 {
                more |= server.want_data(words)? > self.queued;
            }
            if status.starved {
                more |= RECEIVE_BYTES.saturating_sub(server.want_room(words)?) < self.held;
            }
            if !more {
                break true;
            }
        };
        self.session.ring(rings);
        Ok(keep)
    }
}

#[derive(Default)]
pub(crate) struct Streams {
    live: BTreeMap<StreamId, Stream>,
    /// The next stream's id: none is used twice.
    next: u64,
    events: VecDeque<StreamEvent>,
}

impl Streams {
    /// Holds `stream` under an id of its own.
    fn hold(&mut self, stream: Stream) -> StreamId {
        let id = StreamId(self.next);
        self.next = self.next.saturating_add(1);
        self.live.insert(id, stream);
        id
    }

    /// Holds a connection a listener's owner accepted, established, its buffers the region of
    /// `session` from here on, what it held moved there. `options` are the ones [tcp] says it
    /// has, `nodelay` among them written into the region's header.
    pub(crate) fn accepted(&mut self, stack: &mut Stack, conn: ConnId, options: Options, session: Box<dyn Session>) -> StreamId {
        session.words()[NODELAY].store(u32::from(options.nodelay), Ordering::Relaxed);
        let mut stream = Stream { conn, session, server: None, deadline: None, held: 0, queued: 0, fin_queued: false, options, rung: false, more_holders: 0 };
        stream.lent(stack);
        self.hold(stream)
    }

    /// The holders past each stream's first, which take places of their own.
    pub(crate) fn more_holders(&self) -> usize {
        self.live.values().map(|stream| stream.more_holders).fold(0, usize::saturating_add)
    }

    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.live.values().filter_map(|stream| stream.deadline).min()
    }
}

impl Node {
    /// An active open to `remote` on `session`'s region, with Nagle's algorithm as its `nodelay`
    /// word says, answered by a [`StreamEvent`] once the handshake ends or `timeout` passes.
    /// Refused, nothing was sent and `session` is dropped.
    pub fn connect(&mut self, now: Instant, remote: Endpoint, timeout: Option<Duration>, session: Box<dyn Session>) -> Result<StreamId, ConnectRefused> {
        if self.room() == 0 {
            return Err(ConnectRefused::Full);
        }
        let conn = self.stack.tcp_connect(now, remote).map_err(ConnectRefused::Stack)?;
        let options = Options { nodelay: session.words()[NODELAY].load(Ordering::Relaxed) != 0, ..Options::default() };
        self.stack.tcp_set_options(now, conn, options);
        let deadline = timeout.map(|within| now.after(within));
        Ok(self.streams.hold(Stream { conn, session, server: None, deadline, held: 0, queued: 0, fin_queued: false, options, rung: false, more_holders: 0 }))
    }

    /// One pass over every stream: netstack calls it once the kernel answered the connections it
    /// watches, `rung` being the streams whose client rang.
    pub fn bridge(&mut self, now: Instant, rung: impl IntoIterator<Item = StreamId>) {
        for id in rung {
            if let Some(stream) = self.streams.live.get_mut(&id) {
                stream.rung = true;
            }
        }
        self.pass(now, false);
    }

    /// A pass over every stream, or over those whose connect is not answered.
    pub(crate) fn pass(&mut self, now: Instant, connects: bool) {
        let Streams { live, events, .. } = &mut self.streams;
        let stack = &mut self.stack;
        live.retain(|id, stream| (connects && stream.server.is_some()) || stream.pass(*id, now, stack, events));
        self.tell_holders();
    }

    /// A holder more for established stream `id`, which takes a place: the stream's peer and
    /// its own end, for the holder to be told.
    pub fn export(&mut self, id: StreamId) -> Result<(Endpoint, Endpoint), ExportRefused> {
        if self.room() == 0 {
            return Err(ExportRefused::Full);
        }
        let stream = self.streams.live.get_mut(&id).filter(|stream| stream.server.is_some()).ok_or(ExportRefused::Gone)?;
        stream.more_holders = stream.more_holders.saturating_add(1);
        Ok(self.stack.tcp_endpoints(stream.conn))
    }

    /// `holder` of stream `id` left. Its last holder's leaving is the stream's ([`Self::left`]).
    /// Another's lets go of the gates it held, rings each holder whose direction is ready, as
    /// the departed holder's skipped rings would have, and runs a pass, which takes up every
    /// cursor it moved.
    pub fn departed(&mut self, now: Instant, id: StreamId, holder: Holder) {
        let Some(stream) = self.streams.live.get_mut(&id) else { return };
        let Some(more) = stream.more_holders.checked_sub(1) else { return self.left(now, id) };
        stream.more_holders = more;
        if let Some(server) = stream.server.as_mut() {
            match server.departed(stream.session.words(), holder) {
                Ok(rings) => stream.session.ring(rings),
                // A word the departure judged is no client's: the pass meets it again, and resets
                // the stream.
                Err(_) => stream.rung = true,
            }
        }
        self.pass(now, false);
    }

    /// The last holder hung up its connection: what it published is queued and [tcp] closes
    /// the connection. A connect not yet answered is answered [`StreamEvent::Closed`].
    pub fn left(&mut self, now: Instant, id: StreamId) {
        let Some(mut stream) = self.streams.live.remove(&id) else { return };
        if stream.server.is_none() {
            self.stack.tcp_abort(now, stream.conn);
            self.streams.events.push_back(StreamEvent::Closed { id });
        } else {
            match stream.moved(now, &mut self.stack) {
                Ok(_) => self.stack.tcp_close(now, stream.conn),
                Err(violation) => {
                    self.stack.tcp_abort(now, stream.conn);
                    self.streams.events.push_back(StreamEvent::Violated { id, violation });
                }
            }
        }
        self.pass(now, false);
    }

    /// The streams the node holds, connecting ones included.
    pub fn streams(&self) -> usize {
        self.streams.live.len()
    }

    /// Connect answers and log lines since the last call.
    pub fn drain_stream_events(&mut self) -> impl Iterator<Item = StreamEvent> + '_ {
        self.streams.events.drain(..)
    }
}
