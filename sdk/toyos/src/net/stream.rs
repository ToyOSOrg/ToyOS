//! A TCP stream: one region and one connection to netstack.
//!
//! **The region is the stream's two TCP buffers**, made by the client and lent
//! with the connect or the accept. [`LAYOUT`] is its one declaration: a header
//! of [`WORDS`] words, each on a 64-byte line of its own, then the receive ring
//! netstack fills and the send ring the client fills. The receive ring's `end`
//! is the peer's FIN, [`RESET`], [`TIMED_OUT`] or [`UNREACHABLE`]; the send
//! ring's is the client's shutdown; [`NODELAY`] is the client's Nagle switch
//! (RFC 9293 §3.7.4). The rest of the header is the session's: the gates, the
//! receiving half's shutdown and the hang-up any holder saw.
//!
//! **The connection carries the request and its answer, then only doorbells
//! and the hang-up** (virtio 1.2 §2.9). netstack rings a reader on the
//! connection and a writer on the stream's bell, so neither takes the other's
//! wake. A doorbell says "look", never "there are bytes".
//!
//! **A stream belongs to the process that connected or accepted it.** The
//! handle the connect or the accept answered reaches its end, and descriptors
//! on it share that one handle (`crate::fd`); a handle with no end here is
//! refused by name and nothing is written on it
//! (`issues/a-stream-cannot-move-between-processes.md`). The end rings and
//! waits through a handle of the connection of its own, and goes once no
//! handle and no call is left. The table is the instance std links: a program
//! that also links `toyos` itself and drives these functions holds ends std
//! cannot see.

use core::fmt;
use core::sync::atomic::{AtomicU32, Ordering};

use toyos_abi::syscall::{self, SyscallError};
use toyos_abi::RawHandle;

use crate::ipc::Connection;
use crate::shm::SharedMemory;
use crate::sync::Lock;
use crate::transport::session::{self, Bells, Doorbell, HungUp, Layout, Mode, Polled, Receiving, Refused as Answer, Sending, Took, Waited};
use crate::transport::stream::Place;
use crate::transport::Span;
use crate::OwnedHandle;

use super::{MsgType, NetError, NetstackConn, TcpAcceptRequest, TcpAcceptResponse, TcpConnectRequest, TcpConnectResponse, TcpSocketId};

pub use crate::transport::session::Way;

// ---- the region ----

/// The region a stream is: four 2 MiB pages.
pub const REGION_BYTES: usize = 8 << 20;
/// The header's words: its first 4 KiB.
pub const WORDS: usize = 1024;
const HEADER_BYTES: usize = WORDS * 4;
/// The receive ring, after the header.
pub const RECEIVE_BYTES: u32 = 4 << 20;
pub const RECEIVE_RING: usize = HEADER_BYTES;
/// The send ring, the rest.
pub const SEND_BYTES: u32 = (4 << 20) - HEADER_BYTES as u32;
pub const SEND_RING: usize = RECEIVE_RING + RECEIVE_BYTES as usize;

/// Word `n` of the protocol's, on a 64-byte line of its own.
const fn line(n: usize) -> usize {
    n * 16
}

/// The client's Nagle's algorithm: any word but 0 turns it off.
pub const NODELAY: usize = line(10);

pub const LAYOUT: Layout<RECEIVE_BYTES, SEND_BYTES, WORDS> = Layout::new::<
    { line(8) },
    { line(9) },
    { line(11) },
    { line(12) },
    { line(13) },
    { line(14) },
>(
    Place::new::<{ line(0) }, { line(1) }, { line(2) }, { line(3) }, RECEIVE_RING>(),
    Place::new::<{ line(4) }, { line(5) }, { line(6) }, { line(7) }, SEND_RING>(),
);

const _: () = assert!(SEND_RING + SEND_BYTES as usize == REGION_BYTES && NODELAY < WORDS);

/// The header, as the one type that may alias words a peer writes.
pub type Words = [AtomicU32; WORDS];

/// netstack's end of a stream.
pub type Server = session::Server<RECEIVE_BYTES, SEND_BYTES, WORDS>;

/// What netstack watches a stream's connection for: the client's rings, and
/// its hang-up, which the connection reads as its end. A connection is no pipe
/// end, so the kernel refuses it `OTHER_END_GONE`.
pub const SERVER_WATCH: u32 = crate::poller::READABLE;

/// The receive ring's `end`: the peer's reset, or netstack's own.
pub const RESET: u32 = 2;
/// The receive ring's `end`: [tcp] gave the connection up, by its user timeout
/// or R2 (RFC 9293 §3.8.3).
pub const TIMED_OUT: u32 = 3;
/// The receive ring's `end`: an ICMP error ended the connection.
pub const UNREACHABLE: u32 = 4;

// ---- the process's ends ----

/// The most streams one process holds: the per-process bound libc's socket
/// table has. An estimate: no measurement sets it.
pub const MAX_STREAMS: usize = 128;
/// The most handles of streams' connections one process holds.
const MAX_HANDLES: usize = 2 * MAX_STREAMS;

type Rx = Receiving<RECEIVE_BYTES, WORDS>;
type Tx = Sending<SEND_BYTES, WORDS>;

/// An end's two halves, each behind its own lock.
struct Halves {
    rx: Lock<Kept<Rx>>,
    tx: Lock<Kept<Tx>>,
}

/// A half, or none. `repr(u8)` puts `Empty` at zero, as it does
/// [`Slot::Free`], so the tables cost a program that makes no stream no
/// initialised data.
#[repr(u8)]
enum Kept<T> {
    Empty,
    Held(T),
}

static HALVES: [Halves; MAX_STREAMS] =
    [const { Halves { rx: Lock::new(Kept::Empty), tx: Lock::new(Kept::Empty) } }; MAX_STREAMS];

static TABLE: Lock<Table> = Lock::new(Table::new());

/// What an end holds besides its halves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct End {
    region: RawHandle,
    /// The region's mapping.
    base: usize,
    /// The end's own handle of the connection, which every call rings and
    /// waits through and only [`free`] closes: a descriptor closed under a call
    /// hangs nothing up.
    connection: RawHandle,
    bell: RawHandle,
    peer: ([u8; 4], u16),
    local_port: u16,
    handles: u32,
    calls: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum Slot {
    Free,
    /// Reserved: by a connect or an accept that has not answered, or by an end
    /// [`free`] is letting go of.
    Making,
    Live(End),
}

/// The process's ends, and the handles each is reached by.
struct Table {
    slots: [Slot; MAX_STREAMS],
    handles: [Option<(RawHandle, usize)>; MAX_HANDLES],
}

impl Table {
    const fn new() -> Self {
        Self { slots: [Slot::Free; MAX_STREAMS], handles: [None; MAX_HANDLES] }
    }

    /// A free slot made [`Slot::Making`], where a handle has room beside it.
    fn reserve(&mut self) -> Option<usize> {
        self.handles.iter().position(Option::is_none)?;
        let slot = self.slots.iter().position(|slot| *slot == Slot::Free)?;
        self.slots[slot] = Slot::Making;
        Some(slot)
    }

    fn unreserve(&mut self, slot: usize) {
        self.slots[slot] = Slot::Free;
    }

    /// The made end, reached by `handle`.
    fn enter(&mut self, slot: usize, end: End, handle: RawHandle) {
        let room = self.handles.iter().position(Option::is_none).expect("a reservation keeps a handle's room");
        self.handles[room] = Some((handle, slot));
        self.slots[slot] = Slot::Live(End { handles: 1, calls: 0, ..end });
    }

    fn find(&self, handle: RawHandle) -> Option<usize> {
        self.handles.iter().flatten().find(|(held, _)| *held == handle).map(|&(_, slot)| slot)
    }

    fn end(&mut self, slot: usize) -> &mut End {
        match &mut self.slots[slot] {
            Slot::Live(end) => end,
            Slot::Free | Slot::Making => unreachable!("a handle names an end that is not live"),
        }
    }

    /// A call through `handle` begins: its end, counted.
    fn call(&mut self, handle: RawHandle) -> Option<(usize, End)> {
        let slot = self.find(handle)?;
        let end = self.end(slot);
        end.calls += 1;
        Some((slot, *end))
    }

    /// A call ends: the end, where it was its last use.
    fn returned(&mut self, slot: usize) -> Option<End> {
        let end = self.end(slot);
        end.calls -= 1;
        self.free_unused(slot)
    }

    /// The end, where nothing reaches it any more. Its slot stays reserved
    /// until [`free`] has cleared its halves, so no new stream takes the slot
    /// and has its halves cleared under it.
    fn free_unused(&mut self, slot: usize) -> Option<End> {
        let end = *self.end(slot);
        (end.handles == 0 && end.calls == 0).then(|| {
            self.slots[slot] = Slot::Making;
            end
        })
    }

    /// `handle` reaches its end no more: `None` for a handle that reached
    /// none, else its end's slot and the end where it was its last use.
    fn forget(&mut self, handle: RawHandle) -> Option<Option<(usize, End)>> {
        let at = self.handles.iter().position(|entry| entry.is_some_and(|(held, _)| held == handle))?;
        let (_, slot) = self.handles[at].take().expect("found");
        self.end(slot).handles -= 1;
        Some(self.free_unused(slot).map(|end| (slot, end)))
    }
}

/// Lets go of an end no handle or call reaches: its halves, its connection,
/// its bell and its region, whose mapping goes with its handle.
fn free(slot: usize, end: End) {
    *HALVES[slot].rx.lock() = Kept::Empty;
    *HALVES[slot].tx.lock() = Kept::Empty;
    syscall::close(end.connection);
    syscall::close(end.bell);
    syscall::close(end.region);
    TABLE.lock().unreserve(slot);
}

/// Why a stream call refused, in the protocol's words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// A handle this process holds no stream end for.
    NotThisProcess(RawHandle),
    /// The call does not wait, and its direction is not ready.
    WouldBlock,
    /// The call's deadline passed.
    TimedOut,
    /// The peer reset the connection, or netstack did.
    Reset,
    /// [tcp] gave the connection up.
    ConnectionTimedOut,
    /// An ICMP error ended the connection.
    Unreachable,
    /// netstack went away, or wrote what no netstack writes.
    Aborted,
    /// The sending half was shut down.
    Shut,
}

impl From<Answer> for Refused {
    fn from(answer: Answer) -> Self {
        match answer {
            Answer::WouldBlock => Self::WouldBlock,
            Answer::TimedOut => Self::TimedOut,
            Answer::Failed(TIMED_OUT) => Self::ConnectionTimedOut,
            Answer::Failed(UNREACHABLE) => Self::Unreachable,
            Answer::Failed(_) => Self::Reset,
            Answer::Shut => Self::Shut,
            Answer::Gone | Answer::Violation(_) => Self::Aborted,
        }
    }
}

/// A raw handle used where a stream's cannot be, by name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Misuse {
    /// Taken as a stream, and this process holds no end for it.
    NotThisProcess(RawHandle),
    /// A stream's, taken as a file.
    AsFile(RawHandle),
    /// A stream's, moved into another process.
    Moved(RawHandle),
}

impl fmt::Display for Misuse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotThisProcess(handle) => write!(
                f,
                "toyos::net: handle {} is no stream of this process's: a stream belongs to the process that connected or \
                 accepted it",
                handle.0
            ),
            Self::AsFile(handle) => {
                write!(f, "toyos::net: handle {} is a stream's, which is read and written through its region, not as a file", handle.0)
            }
            Self::Moved(handle) => {
                write!(f, "toyos::net: handle {} is a stream's, and a stream does not move into another process", handle.0)
            }
        }
    }
}

/// A call's view of its end, for as long as the call is counted.
struct Call {
    slot: usize,
    end: End,
}

impl Call {
    fn words(&self) -> &Words {
        // SAFETY: the region is mapped at `base`, 2 MiB aligned, for as long as
        // a call is counted, and its header is `WORDS` words reached only as
        // atomics.
        unsafe { &*(self.end.base as *const Words) }
    }

    fn halves(&self) -> &'static Halves {
        &HALVES[self.slot]
    }

    fn bells(&self) -> Doorbells {
        Doorbells { connection: self.end.connection, bell: self.end.bell }
    }
}

/// Runs `f` as a call through `handle`, its end kept until it returns.
fn call<R>(handle: RawHandle, f: impl FnOnce(&Call) -> R) -> Result<R, Refused> {
    let (slot, end) = TABLE.lock().call(handle).ok_or(Refused::NotThisProcess(handle))?;
    let answer = f(&Call { slot, end });
    let freed = TABLE.lock().returned(slot);
    if let Some(end) = freed {
        free(slot, end);
    }
    Ok(answer)
}

/// A half reached through its slot's lock.
struct Half<T: 'static>(&'static Lock<Kept<T>>);

impl<T> session::Lock<T> for Half<T> {
    fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        match &mut *self.0.lock() {
            Kept::Held(half) => f(half),
            Kept::Empty => panic!("a live end has both halves"),
        }
    }
}

/// A call's doorbells: the end's own handle of the connection and the
/// stream's bell.
struct Doorbells {
    connection: RawHandle,
    bell: RawHandle,
}

impl Doorbells {
    fn handle(&self, bell: Doorbell) -> RawHandle {
        match bell {
            Doorbell::Connection => self.connection,
            Doorbell::Bell => self.bell,
        }
    }
}

impl Bells for Doorbells {
    /// Nanoseconds since boot.
    type Until = u64;

    fn drain(&self, bell: Doorbell) -> Took {
        let mut took = Took::Nothing;
        let mut bytes = [0u8; 64];
        loop {
            match syscall::read_nonblock(self.handle(bell), &mut bytes) {
                Ok(0) => return Took::Gone,
                Ok(_) => took = Took::Rung,
                Err(SyscallError::WouldBlock) => return took,
                Err(_) => return Took::Gone,
            }
        }
    }

    fn wait(&self, bell: Doorbell, until: Option<u64>) -> Waited {
        let handle = self.handle(bell);
        let mut bytes = [0u8; 64];
        let Some(until) = until else {
            return match syscall::read(handle, &mut bytes) {
                Ok(0) | Err(_) => Waited::Gone,
                Ok(_) => Waited::Rung,
            };
        };
        let left = until.saturating_sub(toyos_abi::clock::nanos_since_boot());
        if left > 0 {
            let poller = crate::poller::Poller::new(1);
            poller.watch_raw(handle, crate::poller::READABLE, 0);
            poller.wait(1, left, |_| {});
        }
        match syscall::read_nonblock(handle, &mut bytes) {
            Ok(0) => Waited::Gone,
            Ok(_) => Waited::Rung,
            Err(SyscallError::WouldBlock) => Waited::TimedOut,
            Err(_) => Waited::Gone,
        }
    }

    fn ring(&self) -> Result<(), HungUp> {
        match syscall::write_nonblock(self.connection, &[1]) {
            // Full is rung already.
            Ok(_) | Err(SyscallError::WouldBlock) => Ok(()),
            Err(_) => Err(HungUp),
        }
    }
}

/// How a call waits: not at all, or until ready or a deadline in nanoseconds
/// since boot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wait {
    Nonblocking,
    Blocking(Option<u64>),
}

impl Wait {
    fn mode(self) -> Mode<u64> {
        match self {
            Self::Nonblocking => Mode::Nonblocking,
            Self::Blocking(until) => Mode::Blocking(until),
        }
    }
}

/// Reads into `buf`: a count of at least one byte, or 0 for the peer's FIN
/// after its last byte or a receiving half shut down.
pub fn read(handle: RawHandle, buf: &mut [u8], wait: Wait) -> Result<usize, Refused> {
    let max = u32::try_from(buf.len()).unwrap_or(u32::MAX);
    call(handle, |call| {
        let base = call.end.base;
        let out = buf.as_mut_ptr();
        let copy = |span: Span, at: usize| {
            // SAFETY: the span lies in the region's receive ring, mapped for the
            // call, and `at + span.len` is at most the `max` bytes of `buf`
            // the read was handed; the region is never `buf`.
            unsafe { core::ptr::copy_nonoverlapping((base + span.offset) as *const u8, out.add(at), span.len) }
        };
        let rx = Half(&call.halves().rx);
        let read = session::read(call.words(), &rx, &call.bells(), wait.mode(), max, copy);
        read.map(|n| n as usize).map_err(Refused::from)
    })?
}

/// Writes from `buf`: a count of at least one byte. A failure netstack stored
/// is answered, and nothing copied, whatever room there is.
pub fn write(handle: RawHandle, buf: &[u8], wait: Wait) -> Result<usize, Refused> {
    let len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
    call(handle, |call| {
        let base = call.end.base;
        let from = buf.as_ptr();
        let copy = |span: Span, at: usize| {
            // SAFETY: as in `read`, with the two sides swapped.
            unsafe { core::ptr::copy_nonoverlapping(from.add(at), (base + span.offset) as *mut u8, span.len) }
        };
        let tx = Half(&call.halves().tx);
        let wrote = session::write(call.words(), &tx, &call.bells(), wait.mode(), len, copy);
        wrote.map(|n| n as usize).map_err(Refused::from)
    })?
}

/// The sending half sends no more: netstack sends the FIN after its last byte.
pub fn shutdown_write(handle: RawHandle) -> Result<(), Refused> {
    call(handle, |call| session::shutdown(call.words(), &Half(&call.halves().tx), &call.bells()))
}

/// The receiving half reads no more: every read answers 0.
pub fn shutdown_read(handle: RawHandle) -> Result<(), Refused> {
    call(handle, |call| session::shutdown_read(call.words(), &Half(&call.halves().rx), &call.bells()))
}

/// Nagle's algorithm off or on, netstack rung to read it.
pub fn set_nodelay(handle: RawHandle, nodelay: bool) -> Result<(), Refused> {
    call(handle, |call| {
        call.words()[NODELAY].store(u32::from(nodelay), Ordering::Relaxed);
        LAYOUT.ring(call.words(), &call.bells());
    })
}

pub fn nodelay(handle: RawHandle) -> Result<bool, Refused> {
    call(handle, |call| call.words()[NODELAY].load(Ordering::Relaxed) != 0)
}

/// The failure that ended the stream, if one did.
pub fn failure(handle: RawHandle) -> Result<Option<Refused>, Refused> {
    call(handle, |call| match LAYOUT.received_end(call.words()) {
        session::OPEN | session::ORDERLY => None,
        code => Some(Refused::from(Answer::Failed(code))),
    })
}

/// The peer's address and port, and this end's port.
pub fn endpoints(handle: RawHandle) -> Result<(([u8; 4], u16), u16), Refused> {
    let mut table = TABLE.lock();
    let slot = table.find(handle).ok_or(Refused::NotThisProcess(handle))?;
    let end = table.end(slot);
    Ok((end.peer, end.local_port))
}

/// The handle a watch of `way` is made on, for reading: the connection for
/// [`Way::Receive`], the bell for [`Way::Send`].
pub fn doorbell(handle: RawHandle, way: Way) -> Result<RawHandle, Refused> {
    call(handle, |call| call.bells().handle(session::doorbell(way)))
}

/// A source registered for the ways named: each one's want armed, and
/// netstack rung if one is ready, so its first event comes.
pub fn register(handle: RawHandle, receive: bool, send: bool) -> Result<(), Refused> {
    call(handle, |call| {
        let (rx, tx) = (Half(&call.halves().rx), Half(&call.halves().tx));
        session::register(call.words(), receive.then_some(&rx), send.then_some(&tx), &call.bells());
    })
}

/// A poll of one way that found it not ready: its end kept until
/// [`unwatch`].
#[must_use]
pub struct Watching {
    slot: usize,
    end: End,
    way: Way,
    watching: session::Watching,
}

/// What a poll's look found.
pub enum Poll {
    Ready,
    /// Not ready: the caller watches [`doorbell`] for reading, then calls
    /// [`unwatch`].
    Watching(Watching),
}

/// A poll of `way`: ready at once, or counted in, drained, armed and looked at
/// again.
pub fn watch(handle: RawHandle, way: Way) -> Result<Poll, Refused> {
    let (slot, end) = TABLE.lock().call(handle).ok_or(Refused::NotThisProcess(handle))?;
    let call = Call { slot, end };
    let polled = match way {
        Way::Receive => session::watch(call.words(), &Half(&call.halves().rx), &call.bells()),
        Way::Send => session::watch(call.words(), &Half(&call.halves().tx), &call.bells()),
    };
    match polled {
        Polled::Watching(watching) => Ok(Poll::Watching(Watching { slot, end, way, watching })),
        Polled::Ready => {
            ended(slot);
            Ok(Poll::Ready)
        }
    }
}

/// The end of a poll [`watch`] counted in: whether its way is ready now.
pub fn unwatch(watching: Watching) -> bool {
    let Watching { slot, end, way, watching } = watching;
    let call = Call { slot, end };
    let ready = match way {
        Way::Receive => session::unwatch(call.words(), &Half(&call.halves().rx), &call.bells(), watching),
        Way::Send => session::unwatch(call.words(), &Half(&call.halves().tx), &call.bells(), watching),
    };
    ended(slot);
    ready
}

fn ended(slot: usize) {
    let freed = TABLE.lock().returned(slot);
    if let Some(end) = freed {
        free(slot, end);
    }
}

// ---- handles ----

/// Whether `handle` reaches one of this process's stream ends. A lookup, and
/// nothing is written.
pub fn is_stream(handle: RawHandle) -> bool {
    TABLE.lock().find(handle).is_some()
}

/// A raw handle taken back as a stream: one this process holds an end for
/// shares it, and any other is refused by name, with nothing written on it.
pub fn adopt(handle: RawHandle) -> Result<(), Misuse> {
    if is_stream(handle) { Ok(()) } else { Err(Misuse::NotThisProcess(handle)) }
}

/// Closes `handle`: of a stream's connection, its end goes with the last
/// handle and the last call; of anything else, the bare `close`.
pub fn let_go(handle: RawHandle) {
    let forgotten = TABLE.lock().forget(handle);
    syscall::close(handle);
    if let Some(Some((slot, end))) = forgotten {
        free(slot, end);
    }
}

// ---- connect and accept ----

/// A stream netstack connected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Connected {
    /// Its connection, which reaches its end.
    pub handle: RawHandle,
    pub local_port: u16,
}

/// An active open to `addr`:`port`, answered once the handshake ends or
/// `timeout_ms` passes, 0 for netstack's own bound, with Nagle's algorithm off
/// from the first segment where `nodelay`. Refused [`NetError::ResourceExhausted`]
/// before anything is made where the process holds [`MAX_STREAMS`].
pub fn connect(addr: [u8; 4], port: u16, timeout_ms: u32, nodelay: bool) -> Result<Connected, NetError> {
    let request = TcpConnectRequest { addr, port, _pad: 0, timeout_ms };
    let opened = open(nodelay, (addr, port), |netstack, region| {
        netstack.request_with_handles([region], MsgType::TcpConnect, &request)?.answer_with_handle::<TcpConnectResponse>()
    })?;
    Ok(Connected { handle: opened.handle, local_port: opened.answer.local_port })
}

/// A stream a listener's accept was handed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Accepted {
    pub handle: RawHandle,
    pub remote_addr: [u8; 4],
    pub remote_port: u16,
    pub local_port: u16,
    /// Whether Nagle's algorithm is off for it, as its listener had it.
    pub nodelay: bool,
}

/// The oldest connection waiting at `listener` as a stream. The caller took
/// one of the listener's wakes first. Refused as [`connect`] is.
pub fn accept(listener: TcpSocketId) -> Result<Accepted, NetError> {
    let request = TcpAcceptRequest { listener: listener.0 };
    let opened = open(false, ([0; 4], 0), |netstack, region| {
        netstack.request_with_handles([region], MsgType::TcpAccept, &request)?.answer_with_handle::<TcpAcceptResponse>()
    })?;
    let answer = opened.answer;
    Ok(Accepted {
        handle: opened.handle,
        remote_addr: answer.remote_addr,
        remote_port: answer.remote_port,
        local_port: answer.local_port,
        nodelay: answer.nodelay != 0,
    })
}

/// A request that made a stream, and its answer.
struct Opened<A> {
    handle: RawHandle,
    answer: A,
}

/// What an answer says of the stream's endpoints.
trait Answered {
    fn local_port(&self) -> u16;
    /// The peer, where the answer names it.
    fn peer(&self) -> Option<([u8; 4], u16)>;
}

impl Answered for TcpConnectResponse {
    fn local_port(&self) -> u16 {
        self.local_port
    }
    fn peer(&self) -> Option<([u8; 4], u16)> {
        None
    }
}

impl Answered for TcpAcceptResponse {
    fn local_port(&self) -> u16 {
        self.local_port
    }
    fn peer(&self) -> Option<([u8; 4], u16)> {
        Some((self.remote_addr, self.remote_port))
    }
}

/// Makes a stream: a slot, a region, then `ask`'s request on a new connection
/// to netstack carrying it, whose answer hands over the bell. Nothing made
/// outlives a refusal.
fn open<A: Answered>(
    nodelay: bool,
    peer: ([u8; 4], u16),
    ask: impl FnOnce(NetstackConn, OwnedHandle) -> Result<(A, RawHandle, Connection), NetError>,
) -> Result<Opened<A>, NetError> {
    let slot = TABLE.lock().reserve().ok_or(NetError::ResourceExhausted)?;
    let made = (|| {
        let region = SharedMemory::create(REGION_BYTES).map_err(|e| match e {
            SyscallError::ResourceExhausted => NetError::ResourceExhausted,
            _ => NetError::Io,
        })?;
        // SAFETY: a fresh region of `REGION_BYTES`, 2 MiB aligned, whose header
        // is reached only as atomics.
        let words = unsafe { &*(region.as_ptr() as *const Words) };
        words[NODELAY].store(u32::from(nodelay), Ordering::Relaxed);
        let (rx, tx) = session::client(words, LAYOUT);
        let lent = region.share().map_err(|_| NetError::Io)?;
        let (answer, bell, connection) = ask(NetstackConn::connect()?, lent)?;
        let bell = OwnedHandle(bell);
        let own = syscall::dup(connection.0.raw()).map_err(|_| NetError::ResourceExhausted)?;
        *HALVES[slot].rx.lock() = Kept::Held(rx);
        *HALVES[slot].tx.lock() = Kept::Held(tx);
        let (region, base) = region.into_raw();
        let peer = answer.peer().unwrap_or(peer);
        let end = End {
            region,
            base: base as usize,
            connection: own,
            bell: bell.into_raw(),
            peer,
            local_port: answer.local_port(),
            handles: 0,
            calls: 0,
        };
        Ok((connection.0.into_raw(), end, answer))
    })();
    let mut table = TABLE.lock();
    match made {
        Ok((handle, end, answer)) => {
            table.enter(slot, end, handle);
            Ok(Opened { handle, answer })
        }
        Err(refused) => {
            table.unreserve(slot);
            Err(refused)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(n: u32) -> RawHandle {
        RawHandle(n)
    }

    fn end() -> End {
        End { region: h(1), base: 0x20_0000, connection: h(3), bell: h(2), peer: ([192, 0, 2, 2], 80), local_port: 49_152, handles: 0, calls: 0 }
    }

    /// The end goes only with its handle and its last call, and so does its
    /// own handle of the connection, which [`free`] closes: the last
    /// descriptor closed under a call hangs the session up only once the call
    /// returns.
    #[test]
    fn an_end_goes_with_its_handle_and_its_last_call() {
        let mut table = Table::new();
        let slot = table.reserve().unwrap();
        table.enter(slot, end(), h(10));
        assert_eq!(table.find(h(10)), Some(slot), "the handle reaches the end");
        let (called, during) = table.call(h(10)).unwrap();
        assert_eq!(during.connection, h(3), "the call rings and waits through the end's own handle");
        assert_eq!(table.forget(h(10)), Some(None), "a call is left");
        assert_eq!(table.find(h(10)), None, "the handle reaches nothing");
        assert_eq!(table.returned(called), Some(End { handles: 0, calls: 0, ..end() }), "the last call frees it");
        assert_eq!(table.reserve(), Some(slot + 1), "a freed slot is not taken before its halves are cleared");
        table.unreserve(slot);
        assert_eq!(table.slots[slot], Slot::Free);
    }

    /// A handle no end was made for is no stream, and forgetting it frees
    /// nothing.
    #[test]
    fn a_handle_no_end_was_made_for_is_no_stream() {
        let mut table = Table::new();
        let slot = table.reserve().unwrap();
        table.enter(slot, end(), h(10));
        assert_eq!((table.find(h(12)), table.call(h(12)), table.forget(h(12))), (None, None, None));
        assert_eq!(table.find(h(10)), Some(slot));
    }

    /// Past [`MAX_STREAMS`] a stream is refused before anything is made, and
    /// a reservation given back is a slot again.
    #[test]
    fn past_the_bound_no_slot_is_reserved() {
        let mut table = Table::new();
        let slots: Vec<usize> = (0..MAX_STREAMS).map(|_| table.reserve().unwrap()).collect();
        assert_eq!(table.reserve(), None);
        table.unreserve(slots[7]);
        assert_eq!(table.reserve(), Some(slots[7]));
    }
}
