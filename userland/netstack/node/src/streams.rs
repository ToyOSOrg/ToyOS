//! Streams: a connection of [tcp]'s and the two pipes its client reads and writes it by. The pipe
//! ends are traits, so the kernel's calls stay in netstack and the tests fake them.
//!
//! **The node holds a stream for as long as it holds one of its pipes.** A pass
//! ([`Node::bridge`]) moves what each side takes and lets go of an end that is finished: the
//! to-client end once the peer's FIN or a failure has been read through it or its reader is gone,
//! the from-client end once its last byte is queued and the FIN after it, or the connection is
//! over. With neither end left the node closes the connection, which [tcp] then finishes alone
//! by its own rules for a user who let go (a reset if text is unread, RFC 9293 §3.6.1), and the
//! stream's id names nothing.
//!
//! **Nothing leaves the stack that the client's pipe did not take**, and **nothing leaves the
//! pipe that the stack will not take**: a byte moved is a byte acknowledged to whoever sent it,
//! so one the other side refused would be cut out of the middle of the stream with nothing
//! saying so. What a full pipe refuses stays in [tcp], whose window closes on the peer; what
//! [tcp] has no room for stays in the pipe, which blocks the client.
//!
//! **A client that is gone leaves [`OWNERLESS_LIFE`] to its unsent bytes.** Its writer gone and
//! nobody reading, the bytes its pipe still holds are sent as [tcp] makes room, and the
//! connection is reset if they have not all been taken by then, counted from the client's
//! leaving and not from the peer's last word (RFC 9293 §3.8.6.1 lets a connection its peer
//! holds shut be reclaimed).
//!
//! Every call that can move a stream ends in a pass: a frame, a deadline, and each call here.

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use core::time::Duration;

use toyos_net_shard::ConnectError;
use toyos_net_tcp::{ConnId, Endpoint, Error, Failure, Options, Received, State};
use toyos_net_wire::{Instant, Port};

use crate::lease::Stack;
use crate::Node;

/// How long a departed client's unsent bytes have: R2, the time RFC 9293 §3.8.3 gives a
/// segment's retransmission before the connection is closed, at the 100 seconds it asks for at
/// least.
const OWNERLESS_LIFE: Duration = Duration::from_secs(100);

/// The most one read of a client's pipe takes.
const CHUNK: usize = 4096;

/// Why a pipe end took or gave nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PipeRefusal {
    /// Full, to a write; empty with a writer left, to a read.
    WouldBlock,
    /// Nobody holds the other end.
    Gone,
    /// Anything else: the handle is not the pipe end its client said it was.
    Broken,
}

/// The write end of the pipe a client reads its stream from. Dropped, the client reads the end.
pub trait ToClient {
    /// Takes a prefix of `bytes` and answers its length.
    fn write(&mut self, bytes: &[u8]) -> Result<usize, PipeRefusal>;
}

/// The read end of the pipe a client writes its stream to. Dropped, the client's writes fail.
pub trait FromClient {
    /// Fills a prefix of `out` and answers its length. 0 is the end: the pipe is empty and no
    /// writer is left.
    fn read(&mut self, out: &mut [u8]) -> Result<usize, PipeRefusal>;
}

/// Why a connect made no stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectRefused {
    /// The node holds all it has places for (`places`).
    Full,
    Stack(ConnectError),
}

/// The two ends a client's connect or accept hands over.
pub struct Pipes {
    pub to_client: Box<dyn ToClient>,
    pub from_client: Box<dyn FromClient>,
}

/// One of a stream's two pipe ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PipeEnd {
    ToClient,
    FromClient,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StreamId(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamEvent {
    /// The answer to a connect: established, from this port.
    Connected { id: StreamId, local: Port },
    /// The answer to a connect: the peer refused it or [tcp] gave it up.
    Failed { id: StreamId, failure: Failure },
    /// The answer to a connect: its deadline passed first.
    TimedOut { id: StreamId },
    /// The answer to a connect: its stream was closed first.
    Closed { id: StreamId },
    /// A line for the log: reset at [`OWNERLESS_LIFE`], its client gone and its bytes unsent.
    Cut { id: StreamId },
}

/// What netstack asks the kernel about an established stream's pipes until the next pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Watch {
    /// Bytes in the from-client pipe: [tcp] has room for them.
    pub readable: bool,
    /// The from-client pipe's writer leaving, reported by [`Node::pipe_gone`].
    pub writer: bool,
    /// Room in the to-client pipe: it refused bytes [tcp] still holds.
    pub writable: bool,
    /// The to-client pipe's reader leaving, reported by [`Node::pipe_gone`].
    pub reader: bool,
}

struct Stream {
    conn: ConnId,
    to_client: Option<Box<dyn ToClient>>,
    from_client: Option<Box<dyn FromClient>>,
    /// The connect is not answered yet, and no byte moves.
    connecting: bool,
    /// The connect's, while connecting; then the cut's, once the client is gone.
    deadline: Option<Instant>,
    /// The client writes no more: its writer is gone, it shut its sending half down or it
    /// closed. An empty pipe is then the end.
    done_writing: bool,
    /// The to-client pipe refused bytes [tcp] holds.
    held: bool,
    /// [tcp] had room for the client's bytes when the last pass ended.
    room: bool,
    nodelay: bool,
}

impl Stream {
    /// An established connection on its client's pipes.
    fn established(conn: ConnId, pipes: Pipes) -> Self {
        Self {
            conn,
            to_client: Some(pipes.to_client),
            from_client: Some(pipes.from_client),
            connecting: false,
            deadline: None,
            done_writing: false,
            held: false,
            room: false,
            nodelay: false,
        }
    }

    /// One pass. `false` lets the stream go: its pipe ends drop with it, and its connection is
    /// [tcp]'s alone or gone.
    fn pass(&mut self, id: StreamId, now: Instant, stack: &mut Stack, events: &mut VecDeque<StreamEvent>) -> bool {
        let conn = self.conn;
        if self.connecting {
            let status = stack.status(conn);
            let event = match (status.state, status.failure) {
                (_, Some(failure)) => StreamEvent::Failed { id, failure },
                (State::SynSent | State::SynReceived, None) if self.deadline.is_some_and(|at| at <= now) => StreamEvent::TimedOut { id },
                (State::SynSent | State::SynReceived, None) => return true,
                (_, None) => StreamEvent::Connected { id, local: stack.local_port(conn) },
            };
            events.push_back(event);
            if !matches!(event, StreamEvent::Connected { .. }) {
                stack.abort(now, conn);
                return false;
            }
            self.connecting = false;
            self.deadline = None;
        }

        self.held = false;
        while let Some(pipe) = self.to_client.as_mut() {
            let mut refused = None;
            let received = stack.stream_recv(now, conn, |bytes| match pipe.write(bytes) {
                Ok(taken) => taken,
                Err(refusal) => {
                    refused = Some(refusal);
                    0
                }
            });
            match (received, refused) {
                (Ok(Received::Data(taken)), None) if taken > 0 => {}
                (Ok(Received::Data(_)), None | Some(PipeRefusal::WouldBlock)) => {
                    self.held = true;
                    break;
                }
                (Ok(Received::Data(_)), Some(PipeRefusal::Gone)) => self.to_client = None,
                (Ok(Received::Data(_)), Some(PipeRefusal::Broken)) => {
                    stack.abort(now, conn);
                    return false;
                }
                // The peer's FIN after its last byte, or the connection's failure: the client
                // reads the end.
                (Ok(Received::End) | Err(Error::Failed(_)), _) => self.to_client = None,
                (Err(Error::WouldBlock), _) => break,
                (Err(refusal), _) => unreachable!("[tcp] refused the holder of a connection a read: {refusal:?}"),
            }
        }

        while let Some(pipe) = self.from_client.as_mut() {
            // Never more than [tcp] has room for, and so never a read of no bytes, whose answer
            // would be the end's.
            let room = stack.status(conn).writable.min(CHUNK);
            if room == 0 {
                break;
            }
            let mut chunk = [0u8; CHUNK];
            let Some(space) = chunk.get_mut(..room) else { unreachable!("room is at most a chunk") };
            let read = match pipe.read(space) {
                Ok(read) => read,
                Err(PipeRefusal::WouldBlock) if self.done_writing => 0,
                Err(PipeRefusal::WouldBlock) => break,
                Err(PipeRefusal::Gone | PipeRefusal::Broken) => {
                    stack.abort(now, conn);
                    return false;
                }
            };
            if read == 0 {
                stack.shutdown_write(now, conn);
                self.from_client = None;
                break;
            }
            let Some(bytes) = space.get(..read) else { unreachable!("a pipe read {read} bytes into room for {room}") };
            stack.stream_send(now, conn, bytes);
        }

        let status = stack.status(conn);
        // A connection that is over takes no more of the client's bytes: its writes fail rather
        // than fill a pipe nobody drains.
        if matches!(status.state, State::Closed | State::TimeWait) {
            self.from_client = None;
        }
        self.room = status.writable > 0;
        if self.to_client.is_none() && self.from_client.is_none() {
            stack.close(now, conn);
            return false;
        }
        if self.to_client.is_none() && self.done_writing {
            let at = *self.deadline.get_or_insert(now.after(OWNERLESS_LIFE));
            if at <= now {
                stack.abort(now, conn);
                events.push_back(StreamEvent::Cut { id });
                return false;
            }
        }
        true
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

    /// Holds a connection a listener's owner accepted, established, on the pipes its accept
    /// handed over.
    pub(crate) fn accepted(&mut self, conn: ConnId, pipes: Pipes) -> StreamId {
        self.hold(Stream::established(conn, pipes))
    }

    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.live.values().filter_map(|stream| stream.deadline).min()
    }
}

impl Node {
    /// An active open to `remote`, answered by a [`StreamEvent`] once the handshake ends or
    /// `timeout` passes. Refused, nothing was sent and the pipe ends are dropped.
    pub fn connect(&mut self, now: Instant, remote: Endpoint, timeout: Option<Duration>, pipes: Pipes) -> Result<StreamId, ConnectRefused> {
        if self.room() == 0 {
            return Err(ConnectRefused::Full);
        }
        let conn = self.stack.connect(now, remote).map_err(ConnectRefused::Stack)?;
        let deadline = timeout.map(|within| now.after(within));
        Ok(self.streams.hold(Stream { connecting: true, deadline, ..Stream::established(conn, pipes) }))
    }

    /// One pass over every stream: netstack calls it when a pipe it watches is ready.
    pub fn bridge(&mut self, now: Instant) {
        let Streams { live, events, .. } = &mut self.streams;
        let stack = &mut self.stack;
        live.retain(|id, stream| stream.pass(*id, now, stack, events));
        self.wake_owners(now);
    }

    /// The client lets go of the stream: nobody reads it, and what its pipe still holds is sent
    /// before the FIN. A connect not yet answered is answered [`StreamEvent::Closed`].
    pub fn close(&mut self, now: Instant, id: StreamId) {
        let Some(stream) = self.streams.live.get_mut(&id) else { return };
        if stream.connecting {
            self.stack.abort(now, stream.conn);
            self.streams.live.remove(&id);
            self.streams.events.push_back(StreamEvent::Closed { id });
            self.wake_owners(now);
            return;
        }
        stream.to_client = None;
        stream.done_writing = true;
        self.bridge(now);
    }

    /// The client sends no more: what its pipe still holds is sent, then the FIN. `false` is an
    /// id that names no established stream.
    pub fn shutdown_write(&mut self, now: Instant, id: StreamId) -> bool {
        let Some(stream) = self.streams.live.get_mut(&id).filter(|stream| !stream.connecting) else { return false };
        stream.done_writing = true;
        self.bridge(now);
        true
    }

    /// Nagle's algorithm off or on (RFC 9293 §3.7.4). `false` is an id that names no stream.
    pub fn set_nodelay(&mut self, now: Instant, id: StreamId, nodelay: bool) -> bool {
        let Some(stream) = self.streams.live.get_mut(&id) else { return false };
        stream.nodelay = nodelay;
        self.stack.set_options(now, stream.conn, Options { nodelay, ..Options::default() });
        true
    }

    pub fn nodelay(&self, id: StreamId) -> Option<bool> {
        self.streams.live.get(&id).map(|stream| stream.nodelay)
    }

    /// The kernel said nobody holds the other end of one of an established stream's pipes: the
    /// to-client pipe has no reader, or the from-client pipe no writer, whatever it still holds.
    pub fn pipe_gone(&mut self, now: Instant, id: StreamId, end: PipeEnd) {
        let Some(stream) = self.streams.live.get_mut(&id).filter(|stream| !stream.connecting) else { return };
        match end {
            PipeEnd::ToClient => stream.to_client = None,
            PipeEnd::FromClient => stream.done_writing = true,
        }
        self.bridge(now);
    }

    /// The kernel refused netstack's watch of a pipe end the node still holds: the handle is no
    /// pipe end, and its client's connection is reset.
    pub fn pipe_broken(&mut self, now: Instant, id: StreamId, end: PipeEnd) {
        let Some(stream) = self.streams.live.get(&id).filter(|stream| !stream.connecting) else { return };
        let held = match end {
            PipeEnd::ToClient => stream.to_client.is_some(),
            PipeEnd::FromClient => stream.from_client.is_some(),
        };
        if held {
            self.stack.abort(now, stream.conn);
            self.streams.live.remove(&id);
            self.wake_owners(now);
        }
    }

    /// What to ask the kernel about each established stream's pipes, as the last pass left them.
    pub fn watches(&self) -> impl Iterator<Item = (StreamId, Watch)> + '_ {
        self.streams.live.iter().filter(|(_, stream)| !stream.connecting).map(|(id, stream)| {
            let (from, to) = (stream.from_client.is_some(), stream.to_client.is_some());
            (*id, Watch { readable: from && stream.room, writer: from && !stream.done_writing, writable: to && stream.held, reader: to })
        })
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
