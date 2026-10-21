//! A duplex byte stream between a client and a server: two
//! [`stream`](super::stream) rings and two `end` words among `N` words, and
//! two doorbells the server rings.
//!
//! **Nothing here is any protocol's.** The receive ring carries the server's
//! bytes to the client and the send ring the client's to the server. The
//! server's `end` word ends the receive ring and the client's the send ring:
//! [`OPEN`] while the ring runs, [`ORDERLY`] once its producer's last byte is
//! published, and any other value a failure the protocol names, which also
//! makes the client's send direction ready to every look, so a write answers it
//! rather than waiting. An `end` is stored after the last tail with `Release`
//! and loaded before the tail with `Acquire`, so a reader that finds it finds
//! every byte before it.
//!
//! **The client waits on one doorbell a direction** ([`doorbell`]), the one the
//! server rings for that direction's want, so a reader and a writer never take
//! each other's wake. The server waits on one, which every client ring writes.
//!
//! **The client's sequences are written once, here.** Within a direction the
//! half is held to drain, arm, look and copy, and never across a wait. A waiter
//! counts itself in before it drains and out when it returns. A thread that
//! took the doorbell, by a wait or a drain, and leaves the half with another
//! waiter counted arms again, looks, and rings the server if the direction is
//! ready; the server answers by ringing back each ready direction whose want is
//! set ([`Server::rung`]). So a wake one thread took is never one another
//! sleeps over. `WouldBlock` is answered only after the drain, the arm and the
//! look that follows it. A registration ([`register`]) arms each direction it
//! names and rings the server if one is ready, and drains and counts nothing.
//!
//! The adapter supplies the [`Word`]s, the [`Lock`] each half lives behind and
//! the [`Bells`]; it copies bytes through the [`Span`]s it is handed, and blocks
//! only inside [`Bells::wait`].

use core::sync::atomic::Ordering;

use super::stream::{Consumer, Place, Producer, WAKE};
use super::{word, Span, Violation, Word};

/// An `end` while its ring runs.
pub const OPEN: u32 = 0;
/// An `end` after its producer's last byte.
pub const ORDERLY: u32 = 1;

/// A failure: any `end` but [`OPEN`] and [`ORDERLY`].
const fn fatal(end: u32) -> bool {
    end != OPEN && end != ORDERLY
}

/// A direction, as the client names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Way {
    /// The server's bytes to the client.
    Receive,
    /// The client's bytes to the server.
    Send,
}

/// A pipe the server writes and the client waits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Doorbell {
    /// The connection the session was opened on.
    Connection,
    /// A pipe of the session's own.
    Bell,
}

/// The doorbell the server rings for a client waiting on `way`.
pub const fn doorbell(way: Way) -> Doorbell {
    match way {
        Way::Receive => Doorbell::Connection,
        Way::Send if cfg!(feature = "transport-one-doorbell") => Doorbell::Connection,
        Way::Send => Doorbell::Bell,
    }
}

/// Which of the client's doorbells the server is to ring.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[must_use]
pub struct Rings {
    pub connection: bool,
    pub bell: bool,
}

impl Rings {
    fn of(way: Way, rung: bool) -> Self {
        match doorbell(way) {
            Doorbell::Connection => Self { connection: rung, bell: false },
            Doorbell::Bell => Self { connection: false, bell: rung },
        }
    }

    /// Both sets of rings.
    pub fn and(self, other: Self) -> Self {
        Self { connection: self.connection || other.connection, bell: self.bell || other.bell }
    }
}

/// Where a session's two rings and two `end`s are among `N` words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout<const RX: u32, const TX: u32, const N: usize> {
    rx: Place<RX, N>,
    tx: Place<TX, N>,
    rx_end: usize,
    tx_end: usize,
}

impl<const RX: u32, const TX: u32, const N: usize> Layout<RX, TX, N> {
    /// The receive ring at `rx`, the send ring at `tx`, and the two `end`s.
    /// A layout whose words are not ten distinct ones below `N` does not
    /// compile where it is a constant.
    pub const fn new<const RX_END: usize, const TX_END: usize>(rx: Place<RX, N>, tx: Place<TX, N>) -> Self {
        let [a, b, c, d] = rx.words();
        let [e, f, g, h] = tx.words();
        assert!(RX_END < N && TX_END < N, "an end past the words");
        assert!(distinct(&[a, b, c, d, e, f, g, h, RX_END, TX_END]), "a session's ten words are ten");
        Self { rx, tx, rx_end: RX_END, tx_end: TX_END }
    }

    /// The receive ring's `end` as the server last stored it.
    pub fn received_end<W: Word>(&self, page: &[W; N]) -> u32 {
        word(page, self.rx_end).load(Ordering::Acquire)
    }
}

/// Whether no two of `words` are one.
const fn distinct(words: &[usize]) -> bool {
    match words.split_first() {
        None => true,
        Some((first, rest)) => !holds(rest, *first) && distinct(rest),
    }
}

const fn holds(words: &[usize], word: usize) -> bool {
    match words.split_first() {
        None => false,
        Some((first, rest)) => *first == word || holds(rest, word),
    }
}

/// What a client call answers instead of bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// Nonblocking, and the direction is not ready.
    WouldBlock,
    /// The deadline passed first.
    TimedOut,
    /// The server hung up without an `end`.
    Gone,
    /// The receive ring's `end` names this failure.
    Failed(u32),
    /// The client shut its sending half down.
    Shut,
    /// The server wrote what no server of the protocol writes.
    Violation(Violation),
}

impl From<Violation> for Refused {
    fn from(violation: Violation) -> Self {
        Self::Violation(violation)
    }
}

/// What a drain of a doorbell took.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Took {
    Nothing,
    /// One ring or more.
    Rung,
    /// The server holds the doorbell no more.
    Gone,
}

/// How a wait on a doorbell ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Waited {
    /// A ring, taken.
    Rung,
    TimedOut,
    /// The server holds the doorbell no more.
    Gone,
}

/// Whether a call waits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode<U> {
    Nonblocking,
    /// Until the direction is ready or, given one, a deadline.
    Blocking(Option<U>),
}

/// The client's doorbells and its ring of the server.
pub trait Bells {
    /// A deadline.
    type Until: Copy;
    /// Takes every ring the doorbell holds without waiting.
    fn drain(&self, bell: Doorbell) -> Took;
    /// Waits for a ring and takes it.
    fn wait(&self, bell: Doorbell, until: Option<Self::Until>) -> Waited;
    /// Rings the server.
    fn ring(&self);
}

/// The lock a half lives behind: `f` runs holding it.
pub trait Lock<T> {
    fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R;
}

mod sealed {
    pub trait Sealed {}
}

/// One of the client's two halves.
pub trait Half<const N: usize>: sealed::Sealed {
    /// What a look that finds the direction ready saw.
    type Seen: Copy;
    #[doc(hidden)]
    const WAY: Way;
    #[doc(hidden)]
    fn waiters(&mut self) -> &mut u32;
    #[doc(hidden)]
    fn gone(&mut self) -> &mut bool;
    #[doc(hidden)]
    fn look<W: Word>(&mut self, page: &[W; N]) -> Result<Option<Self::Seen>, Violation>;
    /// Its want armed, then a look.
    #[doc(hidden)]
    fn arm<W: Word>(&mut self, page: &[W; N]) -> Result<Option<Self::Seen>, Violation>;
}

/// The client's receiving half.
#[derive(Debug)]
pub struct Receiving<const C: u32, const N: usize> {
    ring: Consumer<C, N>,
    end: usize,
    waiters: u32,
    gone: bool,
}

/// The client's sending half.
#[derive(Debug)]
pub struct Sending<const C: u32, const N: usize> {
    ring: Producer<C, N>,
    /// The server's `end`, which a failure makes ready.
    received: usize,
    end: usize,
    waiters: u32,
    gone: bool,
    shut: bool,
}

/// What a ready receiving half has: bytes, or none and an `end`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Readable {
    ready: u32,
    end: u32,
}

/// What a ready sending half has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Writable {
    Room(u32),
    Failed(u32),
    Shut,
}

impl<const C: u32, const N: usize> sealed::Sealed for Receiving<C, N> {}
impl<const C: u32, const N: usize> sealed::Sealed for Sending<C, N> {}

impl<const C: u32, const N: usize> Receiving<C, N> {
    /// Bytes, or an `end` behind the last of them: the `end` is loaded first.
    fn seen<W: Word>(&mut self, page: &[W; N], armed: Option<u32>) -> Result<Option<Readable>, Violation> {
        let end = word(page, self.end).load(Ordering::Acquire);
        let ready = match armed {
            Some(ready) if end == OPEN => ready,
            _ => self.ring.ready(page)?,
        };
        Ok((ready > 0 || end != OPEN).then_some(Readable { ready, end }))
    }
}

impl<const C: u32, const N: usize> Half<N> for Receiving<C, N> {
    type Seen = Readable;
    const WAY: Way = Way::Receive;

    fn waiters(&mut self) -> &mut u32 {
        &mut self.waiters
    }

    fn gone(&mut self) -> &mut bool {
        &mut self.gone
    }

    fn look<W: Word>(&mut self, page: &[W; N]) -> Result<Option<Readable>, Violation> {
        self.seen(page, None)
    }

    /// The tail the arm read is believed only while no `end` is stored: one
    /// stored after it may follow bytes it did not see.
    fn arm<W: Word>(&mut self, page: &[W; N]) -> Result<Option<Readable>, Violation> {
        let ready = self.ring.arm(page)?;
        self.seen(page, Some(ready))
    }
}

impl<const C: u32, const N: usize> Sending<C, N> {
    fn seen<W: Word>(&self, page: &[W; N], room: u32, failure: bool) -> Option<Writable> {
        if self.shut {
            return Some(Writable::Shut);
        }
        let end = if failure { word(page, self.received).load(Ordering::Acquire) } else { OPEN };
        if fatal(end) {
            return Some(Writable::Failed(end));
        }
        (room > 0).then_some(Writable::Room(room))
    }
}

impl<const C: u32, const N: usize> Half<N> for Sending<C, N> {
    type Seen = Writable;
    const WAY: Way = Way::Send;

    fn waiters(&mut self) -> &mut u32 {
        &mut self.waiters
    }

    fn gone(&mut self) -> &mut bool {
        &mut self.gone
    }

    /// The failure first, so a write after one copies nothing.
    fn look<W: Word>(&mut self, page: &[W; N]) -> Result<Option<Writable>, Violation> {
        if let Some(seen @ (Writable::Shut | Writable::Failed(_))) = self.seen(page, 0, true) {
            return Ok(Some(seen));
        }
        let room = self.ring.room(page)?;
        Ok(self.seen(page, room, true))
    }

    fn arm<W: Word>(&mut self, page: &[W; N]) -> Result<Option<Writable>, Violation> {
        let room = self.ring.arm(page)?;
        Ok(self.seen(page, room, !cfg!(feature = "transport-head-only")))
    }
}

/// The client's two halves, their cursors stored 0.
pub fn client<W: Word, const RX: u32, const TX: u32, const N: usize>(
    page: &[W; N],
    layout: Layout<RX, TX, N>,
) -> (Receiving<RX, N>, Sending<TX, N>) {
    let receiving = Receiving { ring: Consumer::new(page, layout.rx), end: layout.rx_end, waiters: 0, gone: false };
    let sending = Sending {
        ring: Producer::new(page, layout.tx),
        received: layout.rx_end,
        end: layout.tx_end,
        waiters: 0,
        gone: false,
        shut: false,
    };
    (receiving, sending)
}

/// Counts a waiter out and, where it took the doorbell and another waits,
/// arms and looks again: `true` to ring the server for a direction found ready.
fn leave<const N: usize, H: Half<N>, W: Word>(half: &mut H, page: &[W; N], counted: bool, took: bool) -> bool {
    if counted {
        let waiters = half.waiters();
        *waiters = waiters.saturating_sub(1);
    }
    let pass_on = !cfg!(feature = "transport-no-pass-on") && took && *half.waiters() > 0;
    pass_on && matches!(half.arm(page), Ok(Some(_)))
}

/// One look of a waiter under its half: the first look, then, counted in,
/// the drain, the arm and the look after it.
fn look_counted<const N: usize, H: Half<N>, W: Word, B: Bells>(
    half: &mut H,
    page: &[W; N],
    bells: &B,
    counted: &mut bool,
    took: &mut bool,
) -> Result<Option<H::Seen>, Violation> {
    if let Some(seen) = half.look(page)? {
        return Ok(Some(seen));
    }
    if !*counted {
        let waiters = half.waiters();
        *waiters = waiters.saturating_add(1);
        *counted = true;
    }
    match bells.drain(doorbell(H::WAY)) {
        Took::Rung => *took = true,
        Took::Gone => *half.gone() = true,
        Took::Nothing => {}
    }
    half.arm(page)
}

/// A call that waits for its direction: `act` runs under the half once it is
/// ready, and answers the call and whether the server is to be rung.
fn waiting<const N: usize, H, W, L, B, R>(
    page: &[W; N],
    lock: &L,
    bells: &B,
    mode: Mode<B::Until>,
    mut act: impl FnMut(&mut H, H::Seen) -> (Result<R, Refused>, bool),
) -> Result<R, Refused>
where
    H: Half<N>,
    W: Word,
    L: Lock<H>,
    B: Bells,
{
    let (mut took, mut counted, mut expired, mut hung_up) = (false, false, false, false);
    loop {
        let (answer, ring) = lock.with(|half| {
            if hung_up {
                *half.gone() = true;
            }
            let mut ring = false;
            let answer = match look_counted(half, page, bells, &mut counted, &mut took) {
                Err(violation) => Some(Err(Refused::Violation(violation))),
                Ok(Some(seen)) => {
                    let (answer, rung) = act(half, seen);
                    ring = rung;
                    Some(answer)
                }
                Ok(None) if *half.gone() => Some(Err(Refused::Gone)),
                Ok(None) => match mode {
                    Mode::Nonblocking => Some(Err(Refused::WouldBlock)),
                    Mode::Blocking(_) if expired => Some(Err(Refused::TimedOut)),
                    Mode::Blocking(_) => None,
                },
            };
            if answer.is_some() {
                ring |= leave(half, page, counted, took);
            }
            (answer, ring)
        });
        if ring {
            bells.ring();
        }
        if let Some(answer) = answer {
            return answer;
        }
        let Mode::Blocking(until) = mode else { return Err(Refused::WouldBlock) };
        match bells.wait(doorbell(H::WAY), until) {
            Waited::Rung => took = true,
            Waited::TimedOut => expired = true,
            Waited::Gone => hung_up = true,
        }
    }
}

/// Reads at most `max` bytes, each span copied out by `copy` at its offset
/// into the caller's buffer: the count, at least one, or 0 for an orderly end
/// with every byte read. A read of no bytes answers 0 at once.
pub fn read<W, L, B, const C: u32, const N: usize>(
    page: &[W; N],
    lock: &L,
    bells: &B,
    mode: Mode<B::Until>,
    max: u32,
    mut copy: impl FnMut(Span, usize),
) -> Result<u32, Refused>
where
    W: Word,
    L: Lock<Receiving<C, N>>,
    B: Bells,
{
    if max == 0 {
        return Ok(0);
    }
    waiting(page, lock, bells, mode, |half: &mut Receiving<C, N>, seen: Readable| {
        if seen.ready == 0 {
            return (if seen.end == ORDERLY { Ok(0) } else { Err(Refused::Failed(seen.end)) }, false);
        }
        let n = seen.ready.min(max);
        let mut at = 0usize;
        for span in half.ring.span(0, n).into_iter().flatten() {
            copy(span, at);
            at = at.saturating_add(span.len);
        }
        if !half.ring.advance(n) {
            return (Err(Refused::Violation(Violation::TailPastDepth)), false);
        }
        (Ok(n), half.ring.release(page))
    })
}

/// Writes at most `len` bytes, each span copied in by `copy` from its offset
/// into the caller's buffer: the count, at least one. A failure the server
/// stored is answered, and nothing copied, whatever room there is. A write of
/// no bytes answers 0 at once.
pub fn write<W, L, B, const C: u32, const N: usize>(
    page: &[W; N],
    lock: &L,
    bells: &B,
    mode: Mode<B::Until>,
    len: u32,
    mut copy: impl FnMut(Span, usize),
) -> Result<u32, Refused>
where
    W: Word,
    L: Lock<Sending<C, N>>,
    B: Bells,
{
    if len == 0 {
        return Ok(0);
    }
    waiting(page, lock, bells, mode, |half: &mut Sending<C, N>, seen: Writable| match seen {
        Writable::Shut => (Err(Refused::Shut), false),
        Writable::Failed(end) => (Err(Refused::Failed(end)), false),
        Writable::Room(room) => {
            let n = room.min(len);
            let mut at = 0usize;
            for span in half.ring.span(0, n).into_iter().flatten() {
                copy(span, at);
                at = at.saturating_add(span.len);
            }
            if !half.ring.advance(n) {
                return (Err(Refused::Violation(Violation::HeadPastTail)), false);
            }
            (Ok(n), half.ring.publish(page))
        }
    })
}

/// A poll of one direction that found it not ready: counted in until
/// [`unwatch`].
#[derive(Debug)]
#[must_use]
pub struct Watching {
    took: bool,
}

/// What a poll's look found.
#[derive(Debug)]
pub enum Polled {
    Ready,
    /// Not ready: the caller waits on the direction's [`doorbell`], then calls
    /// [`unwatch`].
    Watching(Watching),
}

/// A poll of the direction `H` is: ready at once, or counted in, drained, armed
/// and looked at again.
pub fn watch<W, L, B, H, const N: usize>(page: &[W; N], lock: &L, bells: &B) -> Result<Polled, Violation>
where
    W: Word,
    L: Lock<H>,
    B: Bells,
    H: Half<N>,
{
    let (polled, ring) = lock.with(|half| {
        let (mut counted, mut took) = (false, false);
        let seen = look_counted(half, page, bells, &mut counted, &mut took);
        let polled = match seen {
            Ok(None) if !*half.gone() => return (Ok(Polled::Watching(Watching { took })), false),
            Ok(_) => Ok(Polled::Ready),
            Err(violation) => Err(violation),
        };
        (polled, leave(half, page, counted, took))
    });
    if ring {
        bells.ring();
    }
    polled
}

/// The end of a poll [`watch`] counted in, once its wait is over: whether the
/// direction is ready now.
pub fn unwatch<W, L, B, H, const N: usize>(page: &[W; N], lock: &L, bells: &B, watching: Watching) -> Result<bool, Violation>
where
    W: Word,
    L: Lock<H>,
    B: Bells,
    H: Half<N>,
{
    let (ready, ring) = lock.with(|half| {
        let ring = leave(half, page, true, watching.took);
        let gone = *half.gone();
        (half.look(page).map(|seen| seen.is_some() || gone), ring)
    });
    if ring {
        bells.ring();
    }
    ready
}

/// What a registration found of one half.
fn registered<const N: usize, H: Half<N>, W: Word>(half: &mut H, page: &[W; N]) -> Result<bool, Violation> {
    let seen = if cfg!(feature = "transport-register-no-arm") { half.look(page)? } else { half.arm(page)? };
    Ok(seen.is_some())
}

/// A source registered for the directions whose halves are given: each one's
/// want armed and looked at, and the server rung if one is ready, so the first
/// event comes whether the direction was ready before or not.
pub fn register<W, RL, SL, B, const RX: u32, const TX: u32, const N: usize>(
    page: &[W; N],
    receive: Option<&RL>,
    send: Option<&SL>,
    bells: &B,
) -> Result<(), Violation>
where
    W: Word,
    RL: Lock<Receiving<RX, N>>,
    SL: Lock<Sending<TX, N>>,
    B: Bells,
{
    let mut ready = false;
    if let Some(lock) = receive {
        ready |= lock.with(|half| registered(half, page))?;
    }
    if let Some(lock) = send {
        ready |= lock.with(|half| registered(half, page))?;
    }
    if ready && !cfg!(feature = "transport-register-no-ring") {
        bells.ring();
    }
    Ok(())
}

/// The client sends no more: its `end` stored after its last tail and the
/// server rung, once. A write after it is refused [`Refused::Shut`].
pub fn shutdown<W, L, B, const C: u32, const N: usize>(page: &[W; N], lock: &L, bells: &B)
where
    W: Word,
    L: Lock<Sending<C, N>>,
    B: Bells,
{
    let first = lock.with(|half| {
        let first = !half.shut;
        if first {
            half.shut = true;
            word(page, half.end).store(ORDERLY, Ordering::Release);
        }
        first
    });
    if first {
        bells.ring();
    }
}

/// The server's end: the receive ring's producer and the send ring's consumer.
#[derive(Debug)]
pub struct Server<const RX: u32, const TX: u32, const N: usize> {
    layout: Layout<RX, TX, N>,
    rx: Producer<RX, N>,
    tx: Consumer<TX, N>,
    /// The receive ring's `end` as stored.
    ended: u32,
}

impl<const RX: u32, const TX: u32, const N: usize> Server<RX, TX, N> {
    /// The server's end, its cursors stored 0.
    pub fn new<W: Word>(page: &[W; N], layout: Layout<RX, TX, N>) -> Self {
        Self { layout, rx: Producer::new(page, layout.rx), tx: Consumer::new(page, layout.tx), ended: OPEN }
    }

    /// Bytes published that the client has not released.
    pub fn held<W: Word>(&mut self, page: &[W; N]) -> Result<u32, Violation> {
        Ok(RX.saturating_sub(self.rx.room(page)?))
    }

    /// Publishes `n` more bytes, already in the ring behind the tail.
    /// [`Violation::HeadPastTail`] where the client's head left no room for
    /// them.
    pub fn publish<W: Word>(&mut self, page: &[W; N], n: u32) -> Result<Rings, Violation> {
        self.rx.room(page)?;
        if !self.rx.advance(n) {
            return Err(Violation::HeadPastTail);
        }
        Ok(Rings::of(Way::Receive, self.rx.publish(page)))
    }

    /// Bytes the client published past what this end released.
    pub fn sent<W: Word>(&mut self, page: &[W; N]) -> Result<u32, Violation> {
        self.tx.ready(page)
    }

    /// Gives `n` bytes of the send ring back to the client.
    /// [`Violation::TailPastDepth`] where its tail no longer covers them.
    pub fn release<W: Word>(&mut self, page: &[W; N], n: u32) -> Result<Rings, Violation> {
        self.tx.ready(page)?;
        if !self.tx.advance(n) {
            return Err(Violation::TailPastDepth);
        }
        Ok(Rings::of(Way::Send, self.tx.release(page)))
    }

    /// Whether the client shut its sending half down: loaded before
    /// [`Self::sent`], it says the bytes that call counts are the last.
    pub fn shut<W: Word>(&self, page: &[W; N]) -> bool {
        word(page, self.layout.tx_end).load(Ordering::Acquire) != OPEN
    }

    /// Asks to be rung once the client releases: the client's room after the
    /// arm, from which the caller reads what it released.
    pub fn want_room<W: Word>(&mut self, page: &[W; N]) -> Result<u32, Violation> {
        self.rx.arm(page)
    }

    /// Asks to be rung once the client publishes: what it published after the
    /// arm.
    pub fn want_data<W: Word>(&mut self, page: &[W; N]) -> Result<u32, Violation> {
        self.tx.arm(page)
    }

    /// The receive ring's orderly end, after its last tail, once.
    pub fn finish<W: Word>(&mut self, page: &[W; N]) -> Rings {
        if self.ended != OPEN {
            return Rings::default();
        }
        self.ended = ORDERLY;
        word(page, self.layout.rx_end).store(ORDERLY, Ordering::Release);
        let [_, _, data, _] = self.layout.rx.words();
        Rings::of(Way::Receive, word(page, data).swap(0, WAKE) != 0)
    }

    /// The session's failure `code`, past [`ORDERLY`]: stored after the last
    /// tail, then both client wants exchanged for 0, each that was set rung,
    /// so a client asleep either way wakes and reads why.
    pub fn fail<W: Word>(&mut self, page: &[W; N], code: u32) -> Rings {
        self.ended = code;
        word(page, self.layout.rx_end).store(code, Ordering::Release);
        let [_, _, data, _] = self.layout.rx.words();
        let [_, _, _, room] = self.layout.tx.words();
        let receive = Rings::of(Way::Receive, word(page, data).swap(0, WAKE) != 0);
        receive.and(Rings::of(Way::Send, word(page, room).swap(0, WAKE) != 0))
    }

    /// The client rang: each of its wants whose direction is ready exchanged
    /// for 0, and the doorbells of those that were set.
    pub fn rung<W: Word>(&mut self, page: &[W; N]) -> Result<Rings, Violation> {
        let receive = self.rx.ring_back(page, self.ended != OPEN)?;
        let send = self.tx.ring_back(page, fatal(self.ended) && !cfg!(feature = "transport-head-only"))?;
        Ok(Rings::of(Way::Receive, receive).and(Rings::of(Way::Send, send)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::{Cell, RefCell};
    use core::sync::atomic::AtomicU32;

    const N: usize = 10;
    const C: u32 = 4;
    const LAYOUT: Layout<C, C, N> = Layout::new::<8, 9>(
        Place::new::<0, 1, 2, 3, 0>(),
        Place::new::<4, 5, 6, 7, { C as usize }>(),
    );

    fn page() -> [AtomicU32; N] {
        core::array::from_fn(|_| AtomicU32::new(0))
    }

    /// Doorbells that are never rung and never waited on: a test of one
    /// thread's decisions.
    #[derive(Default)]
    struct Quiet {
        rang: Cell<u32>,
    }

    impl Bells for Quiet {
        type Until = ();
        fn drain(&self, _: Doorbell) -> Took {
            Took::Nothing
        }
        fn wait(&self, _: Doorbell, _: Option<()>) -> Waited {
            unreachable!("a nonblocking call waited")
        }
        fn ring(&self) {
            self.rang.set(self.rang.get() + 1);
        }
    }

    struct Held<T>(RefCell<T>);

    impl<T> Lock<T> for Held<T> {
        fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
            f(&mut self.0.borrow_mut())
        }
    }

    /// RFC 9293 flushes every queue at a reset (§3.10.7.4) and answers a SEND
    /// after it an error (§3.10.2): a write over a send ring with room, after
    /// the server stored a failure, answers the failure and copies nothing.
    #[test]
    fn a_write_with_room_after_a_reset_answers_it_and_copies_nothing() {
        const RESET: u32 = 2;
        let page = page();
        let (_, sending) = client(&page, LAYOUT);
        let mut server = Server::new(&page, LAYOUT);
        let _ = server.fail(&page, RESET);
        let (sending, bells) = (Held(RefCell::new(sending)), Quiet::default());
        let mut copied = 0;
        let wrote = write(&page, &sending, &bells, Mode::Nonblocking, 3, |span, _| copied += span.len);
        assert_eq!(wrote, Err(Refused::Failed(RESET)));
        assert_eq!(copied, 0, "a byte was copied after the reset");
        assert_eq!(server.sent(&page), Ok(0), "a byte was published after the reset");
    }

    /// The `end` behind the last byte: every byte is read, then the end.
    #[test]
    fn a_read_takes_every_byte_before_the_end() {
        let page = page();
        let (receiving, _) = client(&page, LAYOUT);
        let mut server = Server::new(&page, LAYOUT);
        let _ = server.publish(&page, 3);
        let _ = server.finish(&page);
        let (receiving, bells) = (Held(RefCell::new(receiving)), Quiet::default());
        let read = |max| read(&page, &receiving, &bells, Mode::Nonblocking, max, |_, _| {});
        assert_eq!((read(2), read(2), read(2)), (Ok(2), Ok(1), Ok(0)));
    }
}
