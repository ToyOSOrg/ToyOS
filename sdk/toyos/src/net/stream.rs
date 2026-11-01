//! A TCP stream: one region and one connection to netstack.
//!
//! **The region is the stream's two TCP buffers**, made by the client and lent
//! with the connect or the accept. [`LAYOUT`] is its one declaration: a header
//! of [`WORDS`] words, each on a 64-byte line of its own, then the receive ring
//! netstack fills and the send ring the client fills. The receive ring's `end`
//! is the peer's FIN, [`RESET`], [`TIMED_OUT`], [`UNREACHABLE`] or
//! [`ADDRESS_GONE`]; the send
//! ring's is the client's shutdown; [`NODELAY`] is the client's Nagle switch
//! (RFC 9293 §3.7.4). The rest of the header is the session's: the gates, the
//! receiving half's shutdown and the hang-up any holder saw.
//!
//! **A stream has holders, each a process's session**: its connection to
//! netstack, two bells netstack rings, a reader's and a writer's, so neither
//! takes the other's wake, and the waker of the bell netstack watches it on.
//! A doorbell says "look", never "there are bytes" (virtio 1.2 §2.9). The
//! process that connected or accepted is the first holder; [`export`] asks
//! netstack for another, whose connection end a spawn endows and the child
//! takes up (`super::adopt_exported`). An accept is a listener holder's
//! request (`super::socket`), answered with the stream's connection. Every
//! holder shares the one region, and the session's gates make their calls
//! take turns (`crate::transport::session`).
//!
//! **A holder's connection carries frames**: the request and its answer, then
//! [`EXPORT`] from the holder and netstack's answers to it. A holder rings
//! its waker for anything netstack is to look at, and its waker's last handle
//! going is the holder's leaving; before a connect is answered, the first
//! holder holds no waker yet, and its connection's hang-up is its leaving.
//!
//! **A holder stays in its process.** Its connection, bells and region are
//! held without `TRANSFER` once taken up, so no spawn or message can move one;
//! the only way into another process is an export, whose end arrives with
//! `TRANSFER` because the spawn must endow it. Netstack's death or the
//! holder's own is then exact: a hang-up comes once the holder's last thread
//! is out.
//!
//! **A handle with no end here is refused by name** and nothing is written on
//! it. The handle the connect, the accept or the adoption answered reaches its
//! end, and descriptors on it share that one handle (`crate::fd`). The end
//! rings and waits through a handle of the connection of its own, and goes
//! once no handle and no call is left. The table is the instance std links: a
//! program that also links `toyos` itself and drives these functions holds
//! ends std cannot see.

use core::fmt;
use core::sync::atomic::{AtomicU32, Ordering};

use toyos_abi::handle::Rights;
use toyos_abi::syscall::{self, SyscallError};
use toyos_abi::RawHandle;

use super::ends::{close_kept, kept, narrowed, Ends, HELD_BELL, HELD_CONNECTION, HELD_RUNG};

use crate::ipc::{self, Connection, FrameRx};
use crate::shm::{Bytes, SharedMemory};
use crate::sync::Lock;
use crate::transport::session::{self, Bells, Doorbell, Holder, HungUp, Layout, Mode, Polled, Receiving, Refused as Answer, Sending, Took, Waited};
use crate::transport::stream::Place;
use crate::transport::Span;
use crate::{AsHandle, BorrowedHandle, OwnedHandle};

use super::{answer, hangup, MsgType, NetError, NetstackConn, TcpAcceptResponse, TcpConnectRequest, TcpConnectResponse};

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

/// The receive ring's `end`: the peer's reset, or netstack's own.
pub const RESET: u32 = 2;
/// The receive ring's `end`: [tcp] gave the connection up, by its user timeout
/// or R2 (RFC 9293 §3.8.3).
pub const TIMED_OUT: u32 = 3;
/// The receive ring's `end`: an ICMP error ended the connection.
pub const UNREACHABLE: u32 = 4;
/// The receive ring's `end`: this machine gave up the connection's local
/// address, its link's lease ended or another host took it.
pub const ADDRESS_GONE: u32 = 5;

// ---- a holder's connection ----

/// A holder asks for another holder of its stream, and rings: answered
/// [`EXPORTED`] or [`REFUSED`]. One at a time on a holder's connection.
pub const EXPORT: u32 = 2;
/// The answer to an [`EXPORT`], carrying the new holder's connection end,
/// with `TRANSFER`, on which [`ADOPT`] waits.
pub const EXPORTED: u32 = 0x81;
/// The answer to an [`EXPORT`] that made no holder: a `u32`, the pipe ABI's
/// error code.
pub const REFUSED: u32 = 0x82;
/// The first frame on an exported end, carrying [`Adopt`] and, in order, the
/// region and the holder's [`BELLS`].
pub const ADOPT: u32 = 0x83;

/// The bells a connect's or an accept's answer and an [`ADOPT`] carry: the
/// receive bell and the send bell netstack rings, then the waker of the bell
/// netstack watches the holder on. A holder rings it when it moved a cursor,
/// armed a want or asked an [`EXPORT`]; its last handle going is the holder's
/// leaving.
pub const BELLS: usize = 3;

crate::ipc_payload! {
    /// Who an exported end's holder is, and the stream's endpoints, as netstack
    /// keeps them.
    pub struct Adopt {
        pub holder: u16,
        pub peer_port: u16,
        pub peer_addr: [u8; 4],
        pub local_port: u16,
        pub _pad: u16,
    }
}

/// What a holder keeps of its region besides its connection and bells
/// (`super::ends`): it maps it, and duplicates it.
const HELD_REGION: Rights = Rights::DUP.union(Rights::MAP);

// ---- the process's ends ----

/// The most streams one process holds: the per-process bound libc's socket
/// table has. An estimate: no measurement sets it.
pub const MAX_STREAMS: usize = 128;

type Rx = Receiving<RECEIVE_BYTES, WORDS>;
type Tx = Sending<SEND_BYTES, WORDS>;

/// An end's two halves, each behind its own lock, and the turn its export
/// requests take on the connection.
struct Halves {
    rx: Lock<Kept<Rx>>,
    tx: Lock<Kept<Tx>>,
    control: Lock<()>,
}

/// A half, or none. `repr(u8)` puts `Empty` at zero, as the table's own
/// slots have their free one, so the tables cost a program that makes no
/// stream no initialised data.
#[repr(u8)]
enum Kept<T> {
    Empty,
    Held(T),
}

static HALVES: [Halves; MAX_STREAMS] =
    [const { Halves { rx: Lock::new(Kept::Empty), tx: Lock::new(Kept::Empty), control: Lock::new(()) } }; MAX_STREAMS];

static TABLE: Lock<Ends<End, MAX_STREAMS>> = Lock::new(Ends::new());

/// What an end holds besides its halves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct End {
    /// The region and its bells; `None` for an exported end whose netstack
    /// said nothing of it, on which every call answers `Aborted`.
    region: Option<Mapped>,
    /// The end's own handle of the connection, which every call rings and
    /// waits through and only [`free`] closes: a descriptor closed under a call
    /// hangs nothing up.
    connection: BorrowedHandle<'static>,
    peer: ([u8; 4], u16),
    local_port: u16,
}

/// An end's region, its handle and mapping, the bells netstack rings for it
/// and the waker it rings netstack with, kept as [`kept`] says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Mapped {
    handle: RawHandle,
    base: usize,
    receive_bell: BorrowedHandle<'static>,
    send_bell: BorrowedHandle<'static>,
    rung: BorrowedHandle<'static>,
}

/// Lets go of an end no handle or call reaches: its halves, its connection,
/// its bells, and its region's mapping and handle.
fn free(slot: usize, end: End) {
    *HALVES[slot].rx.lock() = Kept::Empty;
    *HALVES[slot].tx.lock() = Kept::Empty;
    let region = end.region.map(|mapped| {
        // SAFETY: the halves that read the mapping are gone, and nothing else
        // reaches it.
        let _ = unsafe { syscall::vm_release(mapped.base as *mut u8, REGION_BYTES) };
        [mapped.receive_bell.raw(), mapped.send_bell.raw(), mapped.rung.raw(), mapped.handle]
    });
    for handle in core::iter::once(end.connection.raw()).chain(region.into_iter().flatten()) {
        // SAFETY: no handle and no call reaches the end, whose own these are.
        unsafe { close_kept(handle) };
    }
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
    /// This machine gave up the connection's local address.
    AddressGone,
    /// netstack went away, or wrote what no netstack writes.
    Aborted,
    /// The sending half was shut down.
    Shut,
}

impl Refused {
    /// The byte `files`' C interface carries it in: `None` for
    /// [`Self::NotThisProcess`], which names a handle no byte holds.
    #[cfg(any(feature = "fd-table", test))]
    pub(crate) fn byte(self) -> Option<u8> {
        Some(match self {
            Self::NotThisProcess(_) => return None,
            Self::WouldBlock => 0,
            Self::TimedOut => 1,
            Self::Reset => 2,
            Self::ConnectionTimedOut => 3,
            Self::Unreachable => 4,
            Self::Aborted => 5,
            Self::Shut => 6,
            Self::AddressGone => 7,
        })
    }

    pub(crate) fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            0 => Self::WouldBlock,
            1 => Self::TimedOut,
            2 => Self::Reset,
            3 => Self::ConnectionTimedOut,
            4 => Self::Unreachable,
            5 => Self::Aborted,
            6 => Self::Shut,
            7 => Self::AddressGone,
            _ => return None,
        })
    }
}

impl From<Answer> for Refused {
    fn from(answer: Answer) -> Self {
        match answer {
            Answer::WouldBlock => Self::WouldBlock,
            Answer::TimedOut => Self::TimedOut,
            Answer::Failed(TIMED_OUT) => Self::ConnectionTimedOut,
            Answer::Failed(UNREACHABLE) => Self::Unreachable,
            Answer::Failed(ADDRESS_GONE) => Self::AddressGone,
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
    /// A stream's own handle, moved into another process.
    Moved(RawHandle),
}

impl fmt::Display for Misuse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotThisProcess(handle) => write!(
                f,
                "toyos::net: handle {} is no stream of this process's: a stream reaches a process by its connect, its \
                 accept, or a spawn that gave it a holder of one",
                handle.0
            ),
            Self::AsFile(handle) => {
                write!(f, "toyos::net: handle {} is a stream's, which is read and written through its region, not as a file", handle.0)
            }
            Self::Moved(handle) => {
                write!(
                    f,
                    "toyos::net: handle {} is a stream's own, which stays in its process: a spawn gives a child a \
                     descriptor of a stream as a holder of its own",
                    handle.0
                )
            }
        }
    }
}

/// A call's view of its end, for as long as the call is counted.
struct Call {
    slot: usize,
    end: End,
    /// The region's mapping.
    base: usize,
}

impl Call {
    fn words(&self) -> &Words {
        // SAFETY: the region is mapped at `base`, 2 MiB aligned, for as long as
        // a call is counted, and its header is `WORDS` words reached only as
        // atomics.
        unsafe { &*(self.base as *const Words) }
    }

    fn halves(&self) -> &'static Halves {
        &HALVES[self.slot]
    }

    fn bells(&self) -> Doorbells {
        let mapped = self.end.region.expect("a call is made only on an end with its region");
        Doorbells { receive: mapped.receive_bell, send: mapped.send_bell, rung: mapped.rung }
    }
}

/// Runs `f` as a call through `handle`, its end kept until it returns. An end
/// with no region answers `Aborted`.
fn call<R>(handle: BorrowedHandle<'_>, f: impl FnOnce(&Call) -> R) -> Result<R, Refused> {
    let (slot, end) = TABLE.lock().call(handle.raw()).ok_or(Refused::NotThisProcess(handle.raw()))?;
    let answer = end.region.map(|mapped| f(&Call { slot, end, base: mapped.base }));
    ended(slot);
    answer.ok_or(Refused::Aborted)
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

/// A call's doorbells, and the waker it rings netstack with.
struct Doorbells {
    receive: BorrowedHandle<'static>,
    send: BorrowedHandle<'static>,
    rung: BorrowedHandle<'static>,
}

impl Doorbells {
    fn handle(&self, bell: Doorbell) -> BorrowedHandle<'static> {
        match bell {
            Doorbell::Receive => self.receive,
            Doorbell::Send => self.send,
        }
    }
}

impl Bells for Doorbells {
    /// Nanoseconds since boot.
    type Until = u64;

    fn drain(&self, bell: Doorbell) -> Took {
        match syscall::event_take(self.handle(bell).raw()) {
            Ok(()) => Took::Rung,
            Err(SyscallError::WouldBlock) => Took::Nothing,
            Err(_) => Took::Gone,
        }
    }

    fn wait(&self, bell: Doorbell, until: Option<u64>) -> Waited {
        let handle = self.handle(bell);
        loop {
            match self.drain(bell) {
                Took::Rung => return Waited::Rung,
                Took::Gone => return Waited::Gone,
                Took::Nothing => {}
            }
            let left = match until {
                None => u64::MAX,
                Some(until) => match until.saturating_sub(toyos_abi::clock::nanos_since_boot()) {
                    0 => return Waited::TimedOut,
                    left => left,
                },
            };
            // Ready or the deadline: the take above says which.
            if syscall::handle_wait(handle.raw(), crate::poller::READABLE, left).is_err() {
                return Waited::Gone;
            }
        }
    }

    fn wait_gate(&self, bell: Doorbell, until: Option<u64>, owner: u16) -> Waited {
        let way = if bell == Doorbell::Receive { Way::Receive } else { Way::Send };
        gate_wait(until, toyos_abi::clock::nanos_since_boot, |by| self.wait(bell, Some(by)), |waited| {
            gate_stalled(way, owner, waited)
        })
    }

    fn ring(&self) -> Result<(), HungUp> {
        match syscall::event_signal(self.rung.raw()) {
            Ok(()) => Ok(()),
            Err(_) => Err(HungUp),
        }
    }
}

/// How long a wait for another holder's gate goes before it is reported as
/// stalled, and between reports. A report, never an end: the call waits as
/// long as its caller asked, and its owner's copy may be stretched by
/// preemption or a page fault on its own buffer, which nothing here can tell
/// from a stall. Policy, chosen as long beside one copy of a ring.
const GATE_STALL_NANOS: u64 = 10_000_000_000;

/// A wait for a gate, until `until` or, given none, until rung: each
/// [`GATE_STALL_NANOS`] it lasts is reported to `stalled`, with how long it
/// has waited, and the wait goes on.
fn gate_wait(
    until: Option<u64>,
    now: impl Fn() -> u64,
    mut wait: impl FnMut(u64) -> Waited,
    mut stalled: impl FnMut(u64),
) -> Waited {
    let started = now();
    let mut report = started.saturating_add(GATE_STALL_NANOS);
    loop {
        let by = until.map_or(report, |until| until.min(report));
        match wait(by) {
            Waited::TimedOut if until.is_some_and(|until| now() >= until) => return Waited::TimedOut,
            Waited::TimedOut => {
                let at = now();
                if at >= report {
                    stalled(at.saturating_sub(started));
                    report = at.saturating_add(GATE_STALL_NANOS);
                }
            }
            answer => return answer,
        }
    }
}

fn gate_stalled(way: Way, owner: u16, waited: u64) {
    let way = if way == Way::Receive { "read" } else { "write" };
    crate::warn!(
        "toyos::net: a stream's {way} has waited {} s for holder {owner}'s gate: that holder's call has not returned",
        waited / 1_000_000_000
    );
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
pub fn read(handle: BorrowedHandle<'_>, buf: &mut [u8], wait: Wait) -> Result<usize, Refused> {
    let max = u32::try_from(buf.len()).unwrap_or(u32::MAX);
    call(handle, |call| {
        let base = call.base;
        let copy = |span: Span, at: usize| {
            // SAFETY: the span lies in the region's receive ring, mapped for the call.
            let ring = unsafe { Bytes::from_raw((base + span.offset) as *mut u8, span.len) };
            ring.read(0, &mut buf[at..at + span.len]);
        };
        let rx = Half(&call.halves().rx);
        let read = session::read(call.words(), &rx, &call.bells(), wait.mode(), max, copy);
        read.map(|n| n as usize).map_err(Refused::from)
    })?
}

/// Writes from `buf`: a count of at least one byte. A failure netstack stored
/// is answered, and nothing copied, whatever room there is.
pub fn write(handle: BorrowedHandle<'_>, buf: &[u8], wait: Wait) -> Result<usize, Refused> {
    let len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
    call(handle, |call| {
        let base = call.base;
        let copy = |span: Span, at: usize| {
            // SAFETY: the span lies in the region's send ring, mapped for the call.
            let ring = unsafe { Bytes::from_raw((base + span.offset) as *mut u8, span.len) };
            ring.write(0, &buf[at..at + span.len]);
        };
        let tx = Half(&call.halves().tx);
        let wrote = session::write(call.words(), &tx, &call.bells(), wait.mode(), len, copy);
        wrote.map(|n| n as usize).map_err(Refused::from)
    })?
}

/// The sending half sends no more: netstack sends the FIN after its last byte.
pub fn shutdown_write(handle: BorrowedHandle<'_>) -> Result<(), Refused> {
    call(handle, |call| session::shutdown(call.words(), &Half(&call.halves().tx), &call.bells()))
}

/// The receiving half reads no more: every read answers 0.
pub fn shutdown_read(handle: BorrowedHandle<'_>) -> Result<(), Refused> {
    call(handle, |call| session::shutdown_read(call.words(), &Half(&call.halves().rx), &call.bells()))
}

/// Nagle's algorithm off or on, netstack rung to read it.
pub fn set_nodelay(handle: BorrowedHandle<'_>, nodelay: bool) -> Result<(), Refused> {
    call(handle, |call| {
        call.words()[NODELAY].store(u32::from(nodelay), Ordering::Relaxed);
        LAYOUT.ring(call.words(), &call.bells());
    })
}

pub fn nodelay(handle: BorrowedHandle<'_>) -> Result<bool, Refused> {
    call(handle, |call| call.words()[NODELAY].load(Ordering::Relaxed) != 0)
}

/// The failure that ended the stream, if one did.
pub fn failure(handle: BorrowedHandle<'_>) -> Result<Option<Refused>, Refused> {
    call(handle, |call| match LAYOUT.received_end(call.words()) {
        session::OPEN | session::ORDERLY => None,
        code => Some(Refused::from(Answer::Failed(code))),
    })
}

/// The peer's address and port, and this end's port.
pub fn endpoints(handle: BorrowedHandle<'_>) -> Result<(([u8; 4], u16), u16), Refused> {
    let end = TABLE.lock().get(handle.raw()).ok_or(Refused::NotThisProcess(handle.raw()))?;
    Ok((end.peer, end.local_port))
}

/// The handle a watch of `way` is made on, for reading: the holder's receive
/// bell or its send bell.
pub fn doorbell<'a>(handle: BorrowedHandle<'a>, way: Way) -> Result<BorrowedHandle<'a>, Refused> {
    call(handle, |call| call.bells().handle(session::doorbell(way)))
}

/// A source registered for the ways named: each one's want armed, and
/// netstack rung if one is ready, so its first event comes.
pub fn register(handle: BorrowedHandle<'_>, receive: bool, send: bool) -> Result<(), Refused> {
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
    base: usize,
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
/// again. An end with no region is ready, and its call answers `Aborted`.
pub fn watch(handle: BorrowedHandle<'_>, way: Way) -> Result<Poll, Refused> {
    let (slot, end) = TABLE.lock().call(handle.raw()).ok_or(Refused::NotThisProcess(handle.raw()))?;
    let Some(Mapped { base, .. }) = end.region else {
        ended(slot);
        return Ok(Poll::Ready);
    };
    let call = Call { slot, end, base };
    let polled = match way {
        Way::Receive => session::watch(call.words(), &Half(&call.halves().rx), &call.bells()),
        Way::Send => session::watch(call.words(), &Half(&call.halves().tx), &call.bells()),
    };
    match polled {
        Polled::Watching(watching) => Ok(Poll::Watching(Watching { slot, end, base, way, watching })),
        Polled::Ready => {
            ended(slot);
            Ok(Poll::Ready)
        }
    }
}

/// Waits on `poller` (capacity 2, kept by the caller across waits) until
/// `way` of the stream `handle` reaches may be ready or `wake` is readable;
/// copies nothing. A wait under another holder's gate reports its stall.
pub fn wait_ready(handle: BorrowedHandle<'_>, way: Way, wake: BorrowedHandle<'_>, poller: &crate::poller::Poller) -> Result<(), Refused> {
    let Poll::Watching(watching) = watch(handle, way)? else { return Ok(()) };
    let bell = Call { slot: watching.slot, end: watching.end, base: watching.base }.bells().handle(session::doorbell(way));
    let wait = |timeout: u64| {
        poller.watch(&bell, crate::poller::READABLE, 0);
        poller.watch(&wake, crate::poller::READABLE, 1);
        let mut answered = false;
        poller.wait(1, timeout, |_| answered = true);
        if answered { Waited::Rung } else { Waited::TimedOut }
    };
    match watching.watching.gate() {
        None => {
            wait(u64::MAX);
        }
        Some(owner) => {
            let now = toyos_abi::clock::nanos_since_boot;
            gate_wait(None, now, |by| wait(by.saturating_sub(now())), |waited| gate_stalled(way, owner, waited));
        }
    }
    unwatch(watching);
    Ok(())
}

/// The end of a poll [`watch`] counted in: whether its way is ready now.
pub fn unwatch(watching: Watching) -> bool {
    let Watching { slot, end, base, way, watching } = watching;
    let call = Call { slot, end, base };
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
pub fn is_stream(handle: BorrowedHandle<'_>) -> bool {
    TABLE.lock().find(handle.raw()).is_some()
}

/// A raw handle taken back as a stream: one this process holds an end for
/// shares it, and any other is refused by name, with nothing written on it.
pub fn adopt(handle: BorrowedHandle<'_>) -> Result<(), Misuse> {
    if is_stream(handle) { Ok(()) } else { Err(Misuse::NotThisProcess(handle.raw())) }
}

/// `handle`, which reaches one of this process's stream ends, as a
/// descriptor's object: let go by this copy of the crate, which holds the end
/// (`crate::fd::STREAM`).
pub fn descriptor(handle: OwnedHandle) -> crate::fd::Object {
    crate::fd::Object::stream(handle, released)
}

/// A stream descriptor's object let go: its handle, by [`let_go`].
extern "C" fn released(word: u64) {
    if let Ok(handle) = u32::try_from(word) {
        // SAFETY: the table gives up a descriptor's handle when it releases
        // its object, and names it no more.
        let_go(unsafe { OwnedHandle::from_raw(RawHandle(handle)) });
    }
}

/// Closes `handle`: of a stream's connection, its end goes with the last
/// handle and the last call; of anything else, the bare `close`.
pub fn let_go(handle: OwnedHandle) {
    let forgotten = TABLE.lock().forget(handle.as_handle().raw());
    drop(handle);
    if let Some(Some((slot, end))) = forgotten {
        free(slot, end);
    }
}

// ---- connect and accept ----

/// A stream netstack connected.
#[derive(Debug)]
pub struct Connected {
    /// Its connection, which reaches its end.
    pub handle: OwnedHandle,
    pub local_port: u16,
}

/// An active open to `addr`:`port`, answered once the handshake ends or
/// `timeout_ms` passes, 0 for netstack's own bound, with Nagle's algorithm off
/// from the first segment where `nodelay`. Refused [`NetError::ResourceExhausted`]
/// before anything is made where the process holds [`MAX_STREAMS`].
pub fn connect(addr: [u8; 4], port: u16, timeout_ms: u32, nodelay: bool) -> Result<Connected, NetError> {
    let request = TcpConnectRequest { addr, port, _pad: 0, timeout_ms };
    let opened = open(nodelay, (addr, port), |region| {
        NetstackConn::connect()?.request_with_handles([region], MsgType::TcpConnect, &request)?.answer_with_handles::<TcpConnectResponse, BELLS>()
    })?;
    Ok(Connected { handle: opened.handle, local_port: opened.answer.local_port })
}

/// A stream a listener's accept was handed.
#[derive(Debug)]
pub struct Accepted {
    pub handle: OwnedHandle,
    pub remote_addr: [u8; 4],
    pub remote_port: u16,
    pub local_port: u16,
    /// Whether Nagle's algorithm is off for it, as its listener had it.
    pub nodelay: bool,
}

/// The oldest connection waiting at the listener whose holder's connection is
/// `listener`, as a stream: its region lent with a `super::socket::ACCEPT` and
/// its connection and bells in the answer, `drain` spent just before the
/// request goes, once the slot and the region are made. Refused as [`connect`]
/// is, and [`NetError::WouldBlock`] where none waits.
pub(super) fn accept(listener: BorrowedHandle<'_>, drain: super::socket::Drain<'_>) -> Result<Accepted, NetError> {
    let opened = open(false, ([0; 4], 0), |region| {
        drain.now().accept(listener, region).map_err(hangup)?;
        let mut rx = FrameRx::<{ super::MAX_ANSWER }>::new();
        let frame = answer(&listener, &mut rx)?;
        let accepted: TcpAcceptResponse = ipc::decode_payload(rx.payload(frame.payload_len)).map_err(hangup)?;
        let [connection, receive, send, rung] = rx.take_handles::<{ BELLS + 1 }>().ok_or(NetError::Protocol(frame.msg_type))?;
        Ok((accepted, [receive, send, rung], Connection::received(connection)))
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
    handle: OwnedHandle,
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

/// Makes a stream: a slot, a region, then `ask`'s request carrying it, whose
/// answer hands over the bells and the stream's connection. The process is the
/// stream's first holder. Nothing made outlives a refusal.
fn open<A: Answered>(
    nodelay: bool,
    peer: ([u8; 4], u16),
    ask: impl FnOnce(OwnedHandle) -> Result<(A, [OwnedHandle; BELLS], Connection), NetError>,
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
        let (answer, bells, connection) = ask(lent)?;
        let (region, base) = region.into_raw();
        // SAFETY: `into_raw` gave this the region's handle, which nothing else answers for.
        let held = take_up(connection.into_owned(), bells, unsafe { OwnedHandle::from_raw(region) }, base)?;
        *HALVES[slot].rx.lock() = Kept::Held(rx);
        *HALVES[slot].tx.lock() = Kept::Held(tx);
        let peer = answer.peer().unwrap_or(peer);
        let end = held.end(base as usize, peer, answer.local_port());
        Ok((held.descriptor, end, answer))
    })();
    let mut table = TABLE.lock();
    match made {
        Ok((handle, end, answer)) => {
            table.enter(slot, end, handle.as_handle().raw());
            Ok(Opened { handle, answer })
        }
        Err(refused) => {
            table.unreserve(slot);
            Err(refused)
        }
    }
}

/// What a holder holds of its stream, each handle narrowed to keep no
/// `TRANSFER`: the descriptor's handle of the connection and the end's own.
struct Held {
    descriptor: OwnedHandle,
    connection: RawHandle,
    receive_bell: RawHandle,
    send_bell: RawHandle,
    rung: RawHandle,
    region: RawHandle,
}

impl Held {
    /// The end, its region mapped at `base`.
    fn end(&self, base: usize, peer: ([u8; 4], u16), local_port: u16) -> End {
        // SAFETY: the handles `take_up` narrowed for this end, which keeps them until its `free`.
        let (receive_bell, send_bell, rung, connection) =
            unsafe { (kept(self.receive_bell), kept(self.send_bell), kept(self.rung), kept(self.connection)) };
        let region = Mapped { handle: self.region, base, receive_bell, send_bell, rung };
        End { region: Some(region), connection, peer, local_port }
    }
}

/// The descriptor's handle and the end's own: `connection` narrowed, then
/// duplicated. Refused, it is closed.
pub(super) fn hold_connection(connection: OwnedHandle) -> Result<(OwnedHandle, RawHandle), NetError> {
    let descriptor = connection.as_handle().try_clone_narrowed(HELD_CONNECTION).map_err(|_| NetError::Io)?;
    drop(connection);
    let own = descriptor.try_clone().map_err(|_| NetError::ResourceExhausted)?;
    Ok((descriptor, own.into_raw()))
}

/// A holder's connection, its bells and its region mapped at `base`, taken up
/// as [`Held`]. Refused, the region is unmapped and every handle closed.
fn take_up(connection: OwnedHandle, bells: [OwnedHandle; BELLS], region: OwnedHandle, base: *mut u8) -> Result<Held, NetError> {
    let connection = hold_connection(connection);
    let [receive, send, rung] = bells;
    let (receive, send, rung) = (narrowed(receive, HELD_BELL), narrowed(send, HELD_BELL), narrowed(rung, HELD_RUNG));
    let region = narrowed(region, HELD_REGION);
    match (connection, receive, send, rung, region) {
        (Ok((descriptor, connection)), Ok(receive_bell), Ok(send_bell), Ok(rung), Ok(region)) => {
            Ok(Held { descriptor, connection, receive_bell, send_bell, rung, region })
        }
        (connection, receive, send, rung, region) => {
            // SAFETY: nothing has read the mapping through this end yet, and
            // nothing else reaches it.
            let _ = unsafe { syscall::vm_release(base, REGION_BYTES) };
            let kept = connection.map(|(_, own)| own).into_iter().chain(receive).chain(send).chain(rung).chain(region);
            // SAFETY: each narrowed here and kept by nothing yet.
            kept.for_each(|handle| unsafe { close_kept(handle) });
            Err(NetError::Io)
        }
    }
}

// ---- export and adoption ----

/// A new holder of the stream `handle` reaches, for another process: the end
/// of its connection, which carries `TRANSFER` so a spawn can endow it, and
/// which `super::adopt_exported` takes up there. netstack gone is
/// [`NetError::NetstackNotFound`]; netstack out of places for it is
/// [`NetError::ResourceExhausted`].
pub fn export(handle: BorrowedHandle<'_>) -> Result<OwnedHandle, NetError> {
    let asked = call(handle, |call| {
        let _turn = call.halves().control.lock();
        let mapped = call.end.region.expect("a call is made only on an end with its region");
        ask_export(call.end.connection, Some(mapped.rung))
    });
    match asked {
        Ok(answer) => answer,
        Err(Refused::Aborted) => Err(NetError::NetstackNotFound),
        Err(_) => Err(NetError::NotConnected),
    }
}

/// A call through `handle` begun and left open, so its end stays until
/// [`release`]: the slot. `None` for a handle that reaches no stream end.
pub(super) fn hold(handle: BorrowedHandle<'_>) -> Option<usize> {
    TABLE.lock().call(handle.raw()).map(|(slot, _)| slot)
}

/// [`export`] of the end a [`hold`] keeps at `slot`.
pub(super) fn export_held(slot: usize) -> Result<OwnedHandle, NetError> {
    let end = TABLE.lock().at(slot).expect("a held end is live");
    if end.region.is_none() {
        return Err(NetError::NetstackNotFound);
    }
    let _turn = HALVES[slot].control.lock();
    ask_export(end.connection, end.region.map(|mapped| mapped.rung))
}

/// The call a [`hold`] began, ended.
pub(super) fn release(slot: usize) {
    ended(slot);
}

/// [`EXPORT`] on a holder's connection, a stream's or a socket's, and
/// netstack's answer to it. A stream's holder rings its `rung` after the
/// frame, since netstack watches that and not the connection.
pub(super) fn ask_export(connection: BorrowedHandle<'_>, rung: Option<BorrowedHandle<'_>>) -> Result<OwnedHandle, NetError> {
    ipc::signal(connection, EXPORT).map_err(hangup)?;
    if let Some(rung) = rung {
        syscall::event_signal(rung.raw()).map_err(|_| NetError::NetstackNotFound)?;
    }
    let mut rx = FrameRx::<4>::new();
    let frame = rx.recv(&connection).map_err(hangup)?;
    match frame.msg_type {
        EXPORTED => match rx.take_handles::<1>() {
            Some([end]) if frame.payload_len == 0 => Ok(end),
            _ => Err(NetError::Protocol(EXPORTED)),
        },
        REFUSED => {
            let code: u32 = ipc::decode_payload(rx.payload(frame.payload_len)).map_err(hangup)?;
            Err(NetError::from_error_code(code))
        }
        other => Err(NetError::Protocol(other)),
    }
}

/// An exported end taken up as this process's holder of its stream, `first`
/// the [`ADOPT`] netstack queued on it (`super::adopt_exported`): its region
/// judged and mapped, and the end, the region and the bells narrowed to keep
/// no `TRANSFER`. The descriptor's handle. Where the frame is missing or
/// malformed, or the region refused, the end is a stream still, every call on
/// which answers `Aborted`. `None`, the end closed, where the process holds
/// [`MAX_STREAMS`] or the kernel will not narrow it.
pub(super) fn adopted(connection: Connection, first: Option<(&[u8], ipc::Batch)>) -> Option<OwnedHandle> {
    let slot = TABLE.lock().reserve()?;
    let taken = first.and_then(adoption).and_then(|(holder, adopt, [region, receive, send, rung])| {
        let region = SharedMemory::lent(region, REGION_BYTES).ok()?;
        let (region, base) = region.into_raw();
        // SAFETY: `into_raw` gave this the region's handle, which nothing else answers for.
        let region = unsafe { OwnedHandle::from_raw(region) };
        Some((holder, adopt, base, [receive, send, rung], region))
    });
    let made = match taken {
        Some((holder, adopt, base, bells, region)) => take_up(connection.into_owned(), bells, region, base).map(|held| {
            let (rx, tx) = session::holder(LAYOUT, holder);
            *HALVES[slot].rx.lock() = Kept::Held(rx);
            *HALVES[slot].tx.lock() = Kept::Held(tx);
            let end = held.end(base as usize, (adopt.peer_addr, adopt.peer_port), adopt.local_port);
            (held.descriptor, end)
        }),
        None => hold_connection(connection.into_owned()).map(|(descriptor, connection)| {
            // SAFETY: the end's own connection, which it keeps until its `free`.
            (descriptor, End { region: None, connection: unsafe { kept(connection) }, peer: ([0; 4], 0), local_port: 0 })
        }),
    };
    let mut table = TABLE.lock();
    match made {
        Ok((descriptor, end)) => {
            table.enter(slot, end, descriptor.as_handle().raw());
            Some(descriptor)
        }
        Err(_) => {
            table.unreserve(slot);
            None
        }
    }
}

/// An [`ADOPT`] frame's holder, payload and handles.
fn adoption((payload, handles): (&[u8], ipc::Batch)) -> Option<(Holder, Adopt, [OwnedHandle; BELLS + 1])> {
    let adopt: Adopt = ipc::decode_payload(payload).ok()?;
    let handles = handles.exact::<{ BELLS + 1 }>()?;
    Some((Holder::new(adopt.holder)?, adopt, handles))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A wait for a gate that lasts is reported each stall interval and goes
    /// on until rung; one with a deadline ends at the deadline, reported only
    /// where it reached a stall interval first.
    #[test]
    fn a_gate_wait_that_lasts_is_reported_and_goes_on() {
        let clock = core::cell::Cell::new(0u64);
        let mut reports = Vec::new();
        let mut waits = 0;
        let answer = gate_wait(None, || clock.get(), |by| {
            waits += 1;
            clock.set(by);
            if waits == 4 { Waited::Rung } else { Waited::TimedOut }
        }, |waited| reports.push(waited));
        assert_eq!(answer, Waited::Rung);
        assert_eq!(reports, [GATE_STALL_NANOS, 2 * GATE_STALL_NANOS, 3 * GATE_STALL_NANOS]);

        clock.set(0);
        let mut reports = Vec::new();
        let until = GATE_STALL_NANOS + GATE_STALL_NANOS / 2;
        let answer = gate_wait(Some(until), || clock.get(), |by| {
            clock.set(by);
            Waited::TimedOut
        }, |waited| reports.push(waited));
        assert_eq!(answer, Waited::TimedOut);
        assert_eq!(reports, [GATE_STALL_NANOS]);

        clock.set(0);
        let early = gate_wait(Some(5), || clock.get(), |by| {
            clock.set(by);
            Waited::TimedOut
        }, |_| panic!("a wait shorter than a stall interval was reported"));
        assert_eq!(early, Waited::TimedOut);
    }
}
