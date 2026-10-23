//! A duplex byte stream between a client and a server: two
//! [`stream`](super::stream) rings and their `end`s, a gate for each, among
//! `N` words, and two doorbells the server rings for each of the client's
//! holders.
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
//! **The client's end may have several [`Holder`]s**, each a process with
//! halves and doorbells of its own and no cursor kept between calls. Only the
//! holder of a direction's gate copies bytes and moves that ring's cursor,
//! resumed from the words under it; the sending half's shutdown is stored
//! under its gate too, so no holder publishes past another's. Everything else
//! the client knows of the stream is a word every holder reads: the `end`s,
//! [`shutdown_read`]'s word, and the server's hang-up once any holder saw it.
//! A look without the gate can only overstate what is ready, so it is no
//! verdict: what it finds is judged again under the gate.
//!
//! **A gate waits on the holder's own doorbell.** A holder that finds its
//! direction ready and the gate another's arms the direction's want, then sets
//! the gate's want, then sleeps; the owner's release rings the server, whose
//! [`Server::rung`] rings back each holder whose direction is ready. A holder
//! that left with the gate held is let go of by [`Server::departed`], which
//! frees the gate and rings the same way. So no wait on a gate outlives the
//! owner's call, its death or the server's; one that waits on a live owner's
//! call is [`Bells::wait_gate`], whose adapter reports it as stalled while it
//! lasts.
//!
//! **A thread inside a gate section runs to its release.** Nothing may suspend
//! it, leave the section by a non-local exit, or re-enter the stream from it:
//! a signal handler, a debugger's stop or a `longjmp` there would stall that
//! direction in every holder of the stream, in every process. A panic that
//! unwinds out of one is the exception the release guard handles.
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

use core::num::NonZeroU16;
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

/// A gate's owner, in its low bits: 0 while it is free.
const OWNER: u32 = 0xFFFF;
/// A holder waits for the gate: its release rings the server.
const WANT: u32 = 1 << 31;
/// A holder shut the sending half down while another held its gate: the owner
/// stores the `end` before it lets go.
const SHUT: u32 = 1 << 30;

/// A departure's exchange of a gate: it reads a waiter's want, and the arm
/// before it, and lets the next owner copy after the departed one's last copy.
const DEPART: Ordering = if cfg!(feature = "transport-departure-relaxed") { Ordering::Relaxed } else { Ordering::AcqRel };

/// One holder of the client's end, by the number its server gave it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Holder(NonZeroU16);

impl Holder {
    /// The end's first holder: the process that made it.
    pub const FIRST: Self = Self(NonZeroU16::MIN);

    /// Holder `number`, which is never 0.
    pub fn new(number: u16) -> Option<Self> {
        NonZeroU16::new(number).map(Self)
    }

    pub fn number(self) -> u16 {
        self.0.get()
    }

    /// The gate's word while this holder owns it.
    fn owning(self) -> u32 {
        u32::from(self.0.get())
    }
}

/// A direction, as the client names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Way {
    /// The server's bytes to the client.
    Receive,
    /// The client's bytes to the server.
    Send,
}

/// A pipe of a holder's that the server writes and the holder waits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Doorbell {
    /// Rung for a reader.
    Receive,
    /// Rung for a writer.
    Send,
}

/// The doorbell the server rings for a holder waiting on `way`.
pub const fn doorbell(way: Way) -> Doorbell {
    match way {
        Way::Receive => Doorbell::Receive,
        Way::Send if cfg!(feature = "transport-one-doorbell") => Doorbell::Receive,
        Way::Send => Doorbell::Send,
    }
}

/// Which of each holder's doorbells the server is to ring.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[must_use]
pub struct Rings {
    pub receive: bool,
    pub send: bool,
}

impl Rings {
    fn of(way: Way, rung: bool) -> Self {
        match doorbell(way) {
            Doorbell::Receive => Self { receive: rung, send: false },
            Doorbell::Send => Self { receive: false, send: rung },
        }
    }

    /// Both sets of rings.
    pub fn and(self, other: Self) -> Self {
        Self { receive: self.receive || other.receive, send: self.send || other.send }
    }
}

/// Where a session's two rings and its six other words are among `N` words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout<const RX: u32, const TX: u32, const N: usize> {
    rx: Place<RX, N>,
    tx: Place<TX, N>,
    rx_end: usize,
    tx_end: usize,
    rx_gate: usize,
    tx_gate: usize,
    read_shut: usize,
    gone: usize,
}

impl<const RX: u32, const TX: u32, const N: usize> Layout<RX, TX, N> {
    /// The receive ring at `rx`, the send ring at `tx`, their `end`s and
    /// gates, [`shutdown_read`]'s word and the word that says the server hung
    /// up. A layout whose words are not fourteen distinct ones below `N` does
    /// not compile where it is a constant.
    pub const fn new<
        const RX_END: usize,
        const TX_END: usize,
        const RX_GATE: usize,
        const TX_GATE: usize,
        const READ_SHUT: usize,
        const GONE: usize,
    >(
        rx: Place<RX, N>,
        tx: Place<TX, N>,
    ) -> Self {
        let [a, b, c, d] = rx.words();
        let [e, f, g, h] = tx.words();
        let own = [RX_END, TX_END, RX_GATE, TX_GATE, READ_SHUT, GONE];
        assert!(below(&own, N), "a word past the words");
        assert!(distinct(&[a, b, c, d, e, f, g, h, RX_END, TX_END, RX_GATE, TX_GATE, READ_SHUT, GONE]), "a session's fourteen words are fourteen");
        Self { rx, tx, rx_end: RX_END, tx_end: TX_END, rx_gate: RX_GATE, tx_gate: TX_GATE, read_shut: READ_SHUT, gone: GONE }
    }

    /// The receive ring's `end` as the server last stored it.
    pub fn received_end<W: Word>(&self, page: &[W; N]) -> u32 {
        word(page, self.rx_end).load(Ordering::Acquire)
    }

    /// Rings the server for a word the adapter wrote, as the client's own
    /// calls ring it.
    pub fn ring<W: Word, B: Bells>(&self, page: &[W; N], bells: &B) {
        ring(page, self.gone, bells);
    }
}

const fn below(words: &[usize], n: usize) -> bool {
    match words.split_first() {
        None => true,
        Some((first, rest)) => *first < n && below(rest, n),
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
    /// Nonblocking, and the direction is not ready, or another holder has its
    /// gate.
    WouldBlock,
    /// The deadline passed first.
    TimedOut,
    /// The server hung up without an `end`.
    Gone,
    /// The receive ring's `end` names this failure.
    Failed(u32),
    /// A holder shut the sending half down.
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

/// The server holds the connection the client rings it on no more.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HungUp;

/// Whether a call waits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode<U> {
    Nonblocking,
    /// Until the direction is ready or, given one, a deadline.
    Blocking(Option<U>),
}

/// A holder's doorbells and its ring of the server.
pub trait Bells {
    /// A deadline.
    type Until: Copy;
    /// Takes every ring the doorbell holds without waiting.
    fn drain(&self, bell: Doorbell) -> Took;
    /// Waits for a ring and takes it.
    fn wait(&self, bell: Doorbell, until: Option<Self::Until>) -> Waited;
    /// Waits as [`Self::wait`] does, for a gate holder `owner` has: what ends
    /// the wait is that holder's copy returning, so one that lasts is stalled
    /// progress, which the adapter reports and waits on through.
    fn wait_gate(&self, bell: Doorbell, until: Option<Self::Until>, owner: u16) -> Waited;
    /// Rings the server.
    fn ring(&self) -> Result<(), HungUp>;
}

/// The lock a half lives behind: `f` runs holding it.
pub trait Lock<T> {
    fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R;
}

mod sealed {
    use super::Holder;

    pub trait Sealed {}

    /// What a half knows of the words every holder shares.
    #[derive(Clone, Copy, Debug)]
    pub struct Common {
        /// The direction's gate.
        pub gate: usize,
        /// The sending half's `end`, which a [`super::SHUT`] in its gate asks
        /// the owner to store; `None` for the receiving half's.
        pub shut: Option<usize>,
        pub gone: usize,
        pub holder: Holder,
    }
}

use sealed::Common;

/// One of a holder's two halves.
pub trait Half<const N: usize>: sealed::Sealed {
    /// What a look that finds the direction ready saw.
    type Seen: Copy;
    #[doc(hidden)]
    const WAY: Way;
    #[doc(hidden)]
    fn waiters(&mut self) -> &mut u32;
    #[doc(hidden)]
    fn common(&self) -> Common;
    /// Its cursor resumed, then a look.
    #[doc(hidden)]
    fn look<W: Word>(&mut self, page: &[W; N]) -> Result<Option<Self::Seen>, Violation>;
    /// Its cursor resumed, its want armed, then a look.
    #[doc(hidden)]
    fn arm<W: Word>(&mut self, page: &[W; N]) -> Result<Option<Self::Seen>, Violation>;
    /// Whether what a look saw is bytes or room to copy, which only the gate's
    /// owner may.
    #[doc(hidden)]
    fn copies(seen: &Self::Seen) -> bool;
}

/// A holder's receiving half.
#[derive(Debug)]
pub struct Receiving<const C: u32, const N: usize> {
    ring: Consumer<C, N>,
    end: usize,
    read_shut: usize,
    common: Common,
    waiters: u32,
}

/// A holder's sending half.
#[derive(Debug)]
pub struct Sending<const C: u32, const N: usize> {
    ring: Producer<C, N>,
    /// The server's `end`, which a failure makes ready.
    received: usize,
    end: usize,
    common: Common,
    waiters: u32,
    /// The control's process-local shutdown, in place of the `end` word.
    #[cfg(feature = "transport-shut-local")]
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
    Gone,
}

impl<const C: u32, const N: usize> sealed::Sealed for Receiving<C, N> {}
impl<const C: u32, const N: usize> sealed::Sealed for Sending<C, N> {}

/// Whether a holder saw the server hang up.
fn gone<W: Word, const N: usize>(page: &[W; N], at: usize) -> bool {
    word(page, at).load(Ordering::Acquire) != 0
}

/// Says to every holder that the server hung up.
fn hang_up<W: Word, const N: usize>(page: &[W; N], at: usize) {
    word(page, at).store(1, Ordering::Release);
}

/// Rings the server, and says so to every holder where it hung up.
fn ring<W: Word, B: Bells, const N: usize>(page: &[W; N], gone: usize, bells: &B) {
    if bells.ring().is_err() {
        hang_up(page, gone);
    }
}

impl<const C: u32, const N: usize> Receiving<C, N> {
    /// Bytes, or an `end` behind the last of them: the `end` is loaded first.
    /// A half a holder shut down reads as an orderly end with nothing before
    /// it.
    fn seen<W: Word>(&mut self, page: &[W; N], armed: Option<u32>) -> Result<Option<Readable>, Violation> {
        if word(page, self.read_shut).load(Ordering::Acquire) != 0 {
            return Ok(Some(Readable { ready: 0, end: ORDERLY }));
        }
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

    fn common(&self) -> Common {
        self.common
    }

    fn look<W: Word>(&mut self, page: &[W; N]) -> Result<Option<Readable>, Violation> {
        self.ring.resume(page)?;
        self.seen(page, None)
    }

    /// The tail the arm read is believed only while no `end` is stored: one
    /// stored after it may follow bytes it did not see.
    fn arm<W: Word>(&mut self, page: &[W; N]) -> Result<Option<Readable>, Violation> {
        self.ring.resume(page)?;
        let ready = self.ring.arm(page)?;
        self.seen(page, Some(ready))
    }

    fn copies(seen: &Readable) -> bool {
        seen.ready > 0
    }
}

impl<const C: u32, const N: usize> Sending<C, N> {
    /// A holder shut the sending half down.
    #[cfg(not(feature = "transport-shut-local"))]
    fn shut<W: Word>(&self, page: &[W; N]) -> bool {
        word(page, self.end).load(Ordering::Acquire) != OPEN
    }

    #[cfg(feature = "transport-shut-local")]
    fn shut<W: Word>(&self, _: &[W; N]) -> bool {
        self.shut
    }

    fn seen<W: Word>(&self, page: &[W; N], room: u32, failure: bool) -> Option<Writable> {
        if self.shut(page) {
            return Some(Writable::Shut);
        }
        if gone(page, self.common.gone) {
            return Some(Writable::Gone);
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

    fn common(&self) -> Common {
        self.common
    }

    /// The failure first, so a write after one copies nothing.
    fn look<W: Word>(&mut self, page: &[W; N]) -> Result<Option<Writable>, Violation> {
        if let Some(seen) = self.seen(page, 0, true) {
            return Ok(Some(seen));
        }
        self.ring.resume(page)?;
        let room = self.ring.room(page)?;
        Ok(self.seen(page, room, true))
    }

    fn arm<W: Word>(&mut self, page: &[W; N]) -> Result<Option<Writable>, Violation> {
        self.ring.resume(page)?;
        let room = self.ring.arm(page)?;
        Ok(self.seen(page, room, !cfg!(feature = "transport-head-only")))
    }

    fn copies(seen: &Writable) -> bool {
        matches!(seen, Writable::Room(_))
    }
}

/// The first holder's two halves, the end's cursors stored 0.
pub fn client<W: Word, const RX: u32, const TX: u32, const N: usize>(
    page: &[W; N],
    layout: Layout<RX, TX, N>,
) -> (Receiving<RX, N>, Sending<TX, N>) {
    let (mut receiving, mut sending) = holder(layout, Holder::FIRST);
    receiving.ring = Consumer::new(page, layout.rx);
    sending.ring = Producer::new(page, layout.tx);
    (receiving, sending)
}

/// The halves of `holder`, which joins an end already made: nothing is stored,
/// as every call resumes its cursors from the words.
pub fn holder<const RX: u32, const TX: u32, const N: usize>(
    layout: Layout<RX, TX, N>,
    holder: Holder,
) -> (Receiving<RX, N>, Sending<TX, N>) {
    let common = |gate, shut| Common { gate, shut, gone: layout.gone, holder };
    let receiving = Receiving {
        ring: Consumer::joining(layout.rx),
        end: layout.rx_end,
        read_shut: layout.read_shut,
        common: common(layout.rx_gate, None),
        waiters: 0,
    };
    let sending = Sending {
        ring: Producer::joining(layout.tx),
        received: layout.rx_end,
        end: layout.tx_end,
        common: common(layout.tx_gate, Some(layout.tx_end)),
        waiters: 0,
        #[cfg(feature = "transport-shut-local")]
        shut: false,
    };
    (receiving, sending)
}

// ---- the gate ----

/// Takes the gate: `Err` with its word where another holder has it.
fn acquire<W: Word, const N: usize>(page: &[W; N], common: Common) -> Result<(), u32> {
    word(page, common.gate).compare_exchange(0, common.holder.owning(), Ordering::Acquire, Ordering::Acquire).map(|_| ())
}

/// Sets `bits` in the word of the gate `owner` holds, after the caller armed
/// its direction's want: the number of the holder whose release answers it,
/// or `None` where the gate was let go first. The exchange is `AcqRel`, so
/// the release that reads it carries the arm before it to the server.
fn want<W: Word, const N: usize>(page: &[W; N], gate: usize, bits: u32, mut owner: u32) -> Option<u16> {
    loop {
        if owner == 0 {
            return None;
        }
        match word(page, gate).compare_exchange(owner, owner | bits, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return Some(u16::try_from(owner & OWNER).unwrap_or(u16::MAX)),
            Err(now) => owner = now,
        }
    }
}

/// Lets the gate go, storing the sending half's `end` first where another
/// holder asked for it: `true` to ring the server, for a holder that waits or
/// that shut the half down.
fn release<W: Word, const N: usize>(page: &[W; N], common: Common) -> bool {
    let mine = common.holder.owning();
    let mut held = mine;
    loop {
        match word(page, common.gate).compare_exchange(held, 0, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return !cfg!(feature = "transport-gate-no-want") && held & (WANT | SHUT) != 0,
            // Another holder wrote the gate as no holder of the protocol does:
            // the stream is that holder's to break, and the server is rung.
            Err(now) if now & OWNER != mine => return true,
            Err(now) => {
                if let Some(end) = common.shut.filter(|_| now & SHUT != 0) {
                    word(page, end).store(ORDERLY, Ordering::Release);
                }
                held = now;
            }
        }
    }
}

/// A gate held for one section, let go by [`Self::release`]. Dropped while
/// unwinding out of the section, it lets go and rings the server whatever the
/// gate said: the section may have moved a cursor it has not rung for.
struct Gated<'a, W: Word, B: Bells, const N: usize> {
    page: &'a [W; N],
    common: Common,
    bells: &'a B,
    held: bool,
}

impl<'a, W: Word, B: Bells, const N: usize> Gated<'a, W, B, N> {
    fn new(page: &'a [W; N], common: Common, bells: &'a B) -> Self {
        Self { page, common, bells, held: true }
    }

    /// `true` to ring the server.
    fn release(mut self) -> bool {
        self.held = false;
        release(self.page, self.common)
    }
}

impl<W: Word, B: Bells, const N: usize> Drop for Gated<'_, W, B, N> {
    fn drop(&mut self) {
        if self.held {
            let _ = release(self.page, self.common);
            ring(self.page, self.common.gone, self.bells);
        }
    }
}

/// What a holder that found its direction ready and the gate another's did.
enum Contended {
    /// It armed its want and set the gate's, which this holder has: it may
    /// sleep.
    Wanted(u16),
    /// The gate was let go first.
    Free,
    /// Its doorbell found the server gone.
    Gone,
}

fn count_in<const N: usize, H: Half<N>>(half: &mut H, counted: &mut bool) {
    if !*counted {
        let waiters = half.waiters();
        *waiters = waiters.saturating_add(1);
        *counted = true;
    }
}

/// Counts in, drains, arms the direction's want and then sets `bits` in the
/// gate `owner` holds: in that order, so the release that reads `bits` finds
/// the want armed, and a release between the two finds the gate free here.
fn contend<const N: usize, H: Half<N>, W: Word, B: Bells>(
    half: &mut H,
    page: &[W; N],
    bells: &B,
    counted: &mut bool,
    took: &mut bool,
    bits: u32,
    owner: u32,
) -> Contended {
    count_in(half, counted);
    let common = half.common();
    match bells.drain(doorbell(H::WAY)) {
        Took::Rung => *took = true,
        Took::Gone => {
            hang_up(page, common.gone);
            return Contended::Gone;
        }
        Took::Nothing => {}
    }
    let arms = !cfg!(feature = "transport-contended-no-arm");
    let arm_first = !cfg!(feature = "transport-want-before-arm");
    if arms && arm_first {
        let _ = half.arm(page);
    }
    let wanted = want(page, common.gate, bits, owner);
    if arms && !arm_first {
        let _ = half.arm(page);
    }
    wanted.map_or(Contended::Free, Contended::Wanted)
}

/// Counts a waiter out and, where it took the doorbell and another waits,
/// arms and looks again: `true` to ring the server for a direction that may be
/// ready.
fn leave<const N: usize, H: Half<N>, W: Word>(half: &mut H, page: &[W; N], counted: bool, took: bool) -> bool {
    if counted {
        let waiters = half.waiters();
        *waiters = waiters.saturating_sub(1);
    }
    let pass_on = !cfg!(feature = "transport-no-pass-on") && took && *half.waiters() > 0;
    pass_on && !matches!(half.arm(page), Ok(None))
}

/// One look of a waiter under its half, without the gate: the first look,
/// then, counted in, the drain, the arm and the look after it.
fn look_counted<const N: usize, H: Half<N>, W: Word, B: Bells>(
    half: &mut H,
    page: &[W; N],
    bells: &B,
    counted: &mut bool,
    took: &mut bool,
) -> Result<Option<H::Seen>, Violation> {
    if let seen @ (Ok(Some(_)) | Err(_)) = half.look(page) {
        return seen;
    }
    count_in(half, counted);
    match bells.drain(doorbell(H::WAY)) {
        Took::Rung => *took = true,
        Took::Gone => hang_up(page, half.common().gone),
        Took::Nothing => {}
    }
    half.arm(page)
}

/// A call that waits for its direction: `act` runs under the half, and under
/// the gate once a look there finds bytes or room to copy, and answers the
/// call and whether the server is to be rung.
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
    let (mut took, mut counted, mut expired) = (false, false, false);
    loop {
        let (answer, ring_server, gone_at, gate) = lock.with(|half| {
            let common = half.common();
            let mut gate = None;
            let unready = |expired: bool| match mode {
                _ if gone(page, common.gone) => Some(Err(Refused::Gone)),
                Mode::Nonblocking => Some(Err(Refused::WouldBlock)),
                Mode::Blocking(_) if expired => Some(Err(Refused::TimedOut)),
                Mode::Blocking(_) => None,
            };
            let mut ring_server = false;
            let answer = loop {
                match look_counted(half, page, bells, &mut counted, &mut took) {
                    Ok(None) => break unready(expired),
                    Ok(Some(seen)) if !H::copies(&seen) => {
                        let (answer, rung) = act(half, seen);
                        ring_server |= rung;
                        break Some(answer);
                    }
                    Err(violation) if cfg!(feature = "transport-ungated-violation") => break Some(Err(violation.into())),
                    // A look without the gate can overstate, and is judged
                    // again under it.
                    Ok(Some(_)) | Err(_) => {}
                }
                if let Err(owner) = acquire(page, common) {
                    match contend(half, page, bells, &mut counted, &mut took, WANT, owner) {
                        Contended::Free => continue,
                        Contended::Gone => break Some(Err(Refused::Gone)),
                        Contended::Wanted(owner) => {
                            gate = Some(owner);
                            break unready(expired);
                        }
                    }
                }
                let gated = Gated::new(page, common, bells);
                match half.look(page) {
                    Err(violation) => {
                        ring_server |= gated.release();
                        break Some(Err(violation.into()));
                    }
                    // Another holder took what the look without the gate saw.
                    Ok(None) => ring_server |= gated.release(),
                    Ok(Some(seen)) => {
                        let (answer, rung) = act(half, seen);
                        ring_server |= gated.release() | rung;
                        break Some(answer);
                    }
                }
            };
            if answer.is_some() {
                ring_server |= leave(half, page, counted, took);
            }
            (answer, ring_server, common.gone, gate)
        });
        if ring_server {
            ring(page, gone_at, bells);
        }
        if let Some(answer) = answer {
            return answer;
        }
        let Mode::Blocking(until) = mode else { return Err(Refused::WouldBlock) };
        let waited = match gate {
            Some(owner) => bells.wait_gate(doorbell(H::WAY), until, owner),
            None => bells.wait(doorbell(H::WAY), until),
        };
        match waited {
            Waited::Rung => took = true,
            Waited::TimedOut => expired = true,
            Waited::Gone => hang_up(page, gone_at),
        }
    }
}

/// Reads at most `max` bytes, each span copied out by `copy` at its offset
/// into the caller's buffer: the count, at least one, or 0 for an orderly end
/// with every byte read, or once a holder shut the receiving half down. A read
/// of no bytes answers 0 at once.
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
/// into the caller's buffer: the count, at least one. A shutdown, the server's
/// hang-up or a failure it stored is answered, and nothing copied, whatever
/// room there is. A write of no bytes answers 0 at once.
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
        Writable::Gone => (Err(Refused::Gone), false),
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
    gate: Option<u16>,
}

impl Watching {
    /// The holder whose gate the direction is ready under, where the watch
    /// waits on its call rather than on the server.
    pub fn gate(&self) -> Option<u16> {
        self.gate
    }
}

/// What a poll's look found.
#[derive(Debug)]
pub enum Polled {
    Ready,
    /// Not ready, or ready under another holder's gate: the caller waits on
    /// the direction's [`doorbell`], then calls [`unwatch`].
    Watching(Watching),
}

/// A poll of the direction `H` is: ready at once, or counted in, drained, armed
/// and looked at again. A direction ready under another holder's gate is
/// watched, its want armed and then the gate's set, so a level-triggered poll
/// does not spin while that holder copies.
pub fn watch<W, L, B, H, const N: usize>(page: &[W; N], lock: &L, bells: &B) -> Polled
where
    W: Word,
    L: Lock<H>,
    B: Bells,
    H: Half<N>,
{
    let (polled, ring_server, gone_at) = lock.with(|half| {
        let common = half.common();
        let (mut counted, mut took, mut gate) = (false, false, None);
        let ready = loop {
            match look_counted(half, page, bells, &mut counted, &mut took) {
                Ok(None) => break gone(page, common.gone),
                Ok(Some(seen)) if !H::copies(&seen) => break true,
                Ok(Some(_)) | Err(_) => {}
            }
            let owner = word(page, common.gate).load(Ordering::Acquire);
            if owner == 0 {
                break true;
            }
            match contend(half, page, bells, &mut counted, &mut took, WANT, owner) {
                Contended::Free => continue,
                Contended::Gone => break true,
                Contended::Wanted(owner) => {
                    gate = Some(owner);
                    break false;
                }
            }
        };
        if !ready {
            return (Polled::Watching(Watching { took, gate }), false, common.gone);
        }
        (Polled::Ready, leave(half, page, counted, took), common.gone)
    });
    if ring_server {
        ring(page, gone_at, bells);
    }
    polled
}

/// The end of a poll [`watch`] counted in, once its wait is over: whether the
/// direction may be ready now. A look without the gate is no verdict, so one
/// that cannot judge says ready, and the call that follows judges.
pub fn unwatch<W, L, B, H, const N: usize>(page: &[W; N], lock: &L, bells: &B, watching: Watching) -> bool
where
    W: Word,
    L: Lock<H>,
    B: Bells,
    H: Half<N>,
{
    let (ready, ring_server, gone_at) = lock.with(|half| {
        let ring_server = leave(half, page, true, watching.took);
        let gone_at = half.common().gone;
        (!matches!(half.look(page), Ok(None)) || gone(page, gone_at), ring_server, gone_at)
    });
    if ring_server {
        ring(page, gone_at, bells);
    }
    ready
}

/// What a registration found of one half: whether to ring the server.
fn registered<const N: usize, H: Half<N>, W: Word>(half: &mut H, page: &[W; N]) -> bool {
    let seen = if cfg!(feature = "transport-register-no-arm") { half.look(page) } else { half.arm(page) };
    match seen {
        Ok(None) => false,
        Ok(Some(seen)) if !H::copies(&seen) => true,
        Ok(Some(_)) | Err(_) => {
            let gate = half.common().gate;
            let owner = word(page, gate).load(Ordering::Acquire);
            want(page, gate, WANT, owner).is_none()
        }
    }
}

/// A source registered for the directions whose halves are given: each one's
/// want armed and looked at, and the server rung if one is ready, so the first
/// event comes whether the direction was ready before or not. A direction
/// ready under another holder's gate sets its want, and the owner's release
/// rings the server instead.
pub fn register<W, RL, SL, B, const RX: u32, const TX: u32, const N: usize>(
    page: &[W; N],
    receive: Option<&RL>,
    send: Option<&SL>,
    bells: &B,
) where
    W: Word,
    RL: Lock<Receiving<RX, N>>,
    SL: Lock<Sending<TX, N>>,
    B: Bells,
{
    let mut ready = None;
    if let Some(lock) = receive {
        let (rung, gone_at) = lock.with(|half| (registered(half, page), half.common.gone));
        ready = ready.or(rung.then_some(gone_at));
    }
    if let Some(lock) = send {
        let (rung, gone_at) = lock.with(|half| (registered(half, page), half.common.gone));
        ready = ready.or(rung.then_some(gone_at));
    }
    if let Some(gone_at) = ready.filter(|_| !cfg!(feature = "transport-register-no-ring")) {
        ring(page, gone_at, bells);
    }
}

/// The holder's sending half sends no more, and neither does any other: its
/// `end` stored under the gate, after the last tail any holder published, and
/// the server rung, once. A write after it is refused [`Refused::Shut`]. Where
/// another holder has the gate, that holder is asked to store the `end` as it
/// lets go, and this call returns once it did.
pub fn shutdown<W, L, B, const C: u32, const N: usize>(page: &[W; N], lock: &L, bells: &B)
where
    W: Word,
    L: Lock<Sending<C, N>>,
    B: Bells,
{
    let (mut took, mut counted) = (false, false);
    loop {
        let (done, ring_server, gone_at, gate) = lock.with(|half: &mut Sending<C, N>| {
            let common = half.common;
            let mut ring_server = false;
            let mut gate = None;
            let done = loop {
                if half.shut(page) || gone(page, common.gone) {
                    break true;
                }
                match acquire(page, common) {
                    Ok(()) => {
                        let gated = Gated::new(page, common, bells);
                        word(page, half.end).store(ORDERLY, Ordering::Release);
                        #[cfg(feature = "transport-shut-local")]
                        {
                            half.shut = true;
                        }
                        let _ = gated.release();
                        ring_server = true;
                        break true;
                    }
                    Err(owner) => match contend(half, page, bells, &mut counted, &mut took, WANT | SHUT, owner) {
                        Contended::Free => continue,
                        Contended::Gone => break true,
                        Contended::Wanted(owner) => {
                            gate = Some(owner);
                            break false;
                        }
                    },
                }
            };
            if done {
                ring_server |= leave(half, page, counted, took);
            }
            (done, ring_server, common.gone, gate)
        });
        if ring_server {
            ring(page, gone_at, bells);
        }
        if done {
            return;
        }
        let waited = match gate {
            Some(owner) => bells.wait_gate(doorbell(Way::Send), None, owner),
            None => bells.wait(doorbell(Way::Send), None),
        };
        match waited {
            Waited::Rung => took = true,
            Waited::TimedOut => {}
            Waited::Gone => hang_up(page, gone_at),
        }
    }
}

/// The holder's receiving half reads no more, and neither does any other:
/// every read answers 0, and the server is rung to wake a reader asleep in
/// any holder.
pub fn shutdown_read<W, L, B, const C: u32, const N: usize>(page: &[W; N], lock: &L, bells: &B)
where
    W: Word,
    L: Lock<Receiving<C, N>>,
    B: Bells,
{
    let (read_shut, gone_at) = lock.with(|half| (half.read_shut, half.common.gone));
    if word(page, read_shut).swap(1, Ordering::Release) == 0 {
        ring(page, gone_at, bells);
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

    /// Whether a holder shut the client's sending half down: loaded before
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

    /// A holder rang: each of the client's wants whose direction is ready
    /// exchanged for 0, and the doorbells of those that were set. A shut
    /// half is ready to the holders that may be asleep on it.
    pub fn rung<W: Word>(&mut self, page: &[W; N]) -> Result<Rings, Violation> {
        let read_shut = word(page, self.layout.read_shut).load(Ordering::Acquire) != 0;
        let receive = self.rx.ring_back(page, self.ended != OPEN || read_shut)?;
        let failed = fatal(self.ended) && !cfg!(feature = "transport-head-only");
        let send = self.tx.ring_back(page, failed || self.shut(page))?;
        Ok(Rings::of(Way::Receive, receive).and(Rings::of(Way::Send, send)))
    }

    /// `holder` left the end: each gate it held let go, the sending half's
    /// `end` stored first where another holder asked it to, and then what
    /// [`Self::rung`] answers, the rings the departed holder may have skipped.
    /// The caller still runs its pass over the cursors the holder moved.
    pub fn departed<W: Word>(&mut self, page: &[W; N], holder: Holder) -> Result<Rings, Violation> {
        let gates = [(self.layout.rx_gate, None), (self.layout.tx_gate, Some(self.layout.tx_end))];
        let failure = if cfg!(feature = "transport-departure-relaxed") { Ordering::Relaxed } else { Ordering::Acquire };
        for (gate, end) in gates {
            let mut held = word(page, gate).load(Ordering::Acquire);
            while held & OWNER == holder.owning() {
                if let Some(end) = end.filter(|_| held & SHUT != 0) {
                    word(page, end).store(ORDERLY, Ordering::Release);
                }
                match word(page, gate).compare_exchange(held, 0, DEPART, failure) {
                    Ok(_) => break,
                    Err(now) => held = now,
                }
            }
        }
        if cfg!(feature = "transport-departure-no-pass") {
            return Ok(Rings::default());
        }
        self.rung(page)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::{Cell, RefCell};
    use core::sync::atomic::AtomicU32;

    const N: usize = 14;
    const C: u32 = 4;
    const RX_GATE: usize = 10;
    const TX_GATE: usize = 11;
    const TX_END: usize = 9;
    const LAYOUT: Layout<C, C, N> = Layout::new::<8, TX_END, RX_GATE, TX_GATE, 12, 13>(
        Place::new::<0, 1, 2, 3, 0>(),
        Place::new::<4, 5, 6, 7, { C as usize }>(),
    );
    /// The send ring's two wants: the server's and the client's.
    const TX_DATA: usize = 6;
    const TX_ROOM: usize = 7;

    fn page() -> [AtomicU32; N] {
        core::array::from_fn(|_| AtomicU32::new(0))
    }

    fn second() -> Holder {
        Holder::new(2).unwrap()
    }

    /// Doorbells that are never rung, and a wait that is the test's `waited`:
    /// a test of one thread's decisions.
    struct Quiet<F: Fn() = fn()> {
        rang: Cell<u32>,
        /// The server hung up: a drain or a ring finds it gone.
        gone: bool,
        waited: Option<F>,
        /// The owner of the gate each wait was for, `None` for a wait that
        /// was for no gate.
        gates: RefCell<Vec<Option<u16>>>,
    }

    fn quiet() -> Quiet {
        Quiet { rang: Cell::new(0), gone: false, waited: None, gates: RefCell::new(Vec::new()) }
    }

    impl<F: Fn()> Bells for Quiet<F> {
        type Until = ();
        fn drain(&self, _: Doorbell) -> Took {
            if self.gone { Took::Gone } else { Took::Nothing }
        }
        fn wait(&self, _: Doorbell, _: Option<()>) -> Waited {
            self.gates.borrow_mut().push(None);
            (self.waited.as_ref().expect("a call that may not wait waited"))();
            Waited::Rung
        }
        fn wait_gate(&self, _: Doorbell, _: Option<()>, owner: u16) -> Waited {
            self.gates.borrow_mut().push(Some(owner));
            (self.waited.as_ref().expect("a call that may not wait waited"))();
            Waited::Rung
        }
        fn ring(&self) -> Result<(), HungUp> {
            self.rang.set(self.rang.get() + 1);
            if self.gone { Err(HungUp) } else { Ok(()) }
        }
    }

    struct Held<T>(RefCell<T>);

    impl<T> Lock<T> for Held<T> {
        fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
            f(&mut self.0.borrow_mut())
        }
    }

    /// One holder's two halves.
    type Halves = (Held<Receiving<C, N>>, Held<Sending<C, N>>);

    /// The first holder's halves and another's, over one page.
    fn two(page: &[AtomicU32; N]) -> (Halves, Halves) {
        let (rx, tx) = client(page, LAYOUT);
        let (other_rx, other_tx) = holder(LAYOUT, second());
        ((Held(RefCell::new(rx)), Held(RefCell::new(tx))), (Held(RefCell::new(other_rx)), Held(RefCell::new(other_tx))))
    }

    fn write_one<B: Bells>(page: &[AtomicU32; N], tx: &Held<Sending<C, N>>, bells: &B, mode: Mode<B::Until>) -> (Result<u32, Refused>, u32) {
        let mut copied = 0;
        let wrote = write(page, tx, bells, mode, 1, |span, _| copied += span.len as u32);
        (wrote, copied)
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
        let (sending, bells) = (Held(RefCell::new(sending)), quiet());
        assert_eq!(write_one(&page, &sending, &bells, Mode::Nonblocking), (Err(Refused::Failed(RESET)), 0));
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
        let (receiving, bells) = (Held(RefCell::new(receiving)), quiet());
        let read = |max| read(&page, &receiving, &bells, Mode::Nonblocking, max, |_, _| {});
        assert_eq!((read(2), read(2), read(2)), (Ok(2), Ok(1), Ok(0)));
    }

    /// A gate is one holder's at a time; a want is set only in a held gate,
    /// and the release that finds one asks for the ring that one holding none
    /// does not.
    #[test]
    fn a_gate_is_one_holders_and_its_release_rings_only_for_a_want() {
        let page = page();
        let common = |holder| Common { gate: TX_GATE, shut: Some(TX_END), gone: 13, holder };
        let (first, other) = (common(Holder::FIRST), common(second()));
        assert_eq!(acquire(&page, first), Ok(()));
        assert_eq!(acquire(&page, other), Err(1), "the first holder's");
        assert!(!release(&page, first), "nobody wanted it");
        assert_eq!(want(&page, TX_GATE, WANT, 0), None, "a want of a free gate");
        assert_eq!(page[TX_GATE].load(Ordering::Relaxed), 0, "a free gate stays free");
        assert_eq!(acquire(&page, other), Ok(()));
        assert_eq!(want(&page, TX_GATE, WANT, 2), Some(2), "the holder the want waits on");
        assert_eq!(page[TX_GATE].load(Ordering::Relaxed), 2 | WANT);
        assert!(release(&page, other), "a holder wanted it");
        assert_eq!(page[TX_GATE].load(Ordering::Relaxed), 0);
    }

    /// A departure frees the gates the departed holder held, its want with
    /// them, stores a shutdown another holder asked of it, and leaves another
    /// holder's gate alone.
    #[test]
    fn a_departure_frees_only_the_departed_holders_gates() {
        let page = page();
        let mut server = Server::new(&page, LAYOUT);
        let _ = client(&page, LAYOUT);
        page[RX_GATE].store(2 | WANT, Ordering::Relaxed);
        page[TX_GATE].store(1, Ordering::Relaxed);
        assert_eq!(server.departed(&page, second()), Ok(Rings::default()), "nobody armed a want");
        assert_eq!(page[RX_GATE].load(Ordering::Relaxed), 0, "the departed holder's gate");
        assert_eq!(page[TX_GATE].load(Ordering::Relaxed), 1, "another holder's gate");
        assert!(!server.shut(&page));
        page[TX_GATE].store(2 | WANT | SHUT, Ordering::Relaxed);
        page[TX_ROOM].store(1, Ordering::Relaxed);
        assert_eq!(server.departed(&page, second()), Ok(Rings { receive: false, send: true }), "the shutdown's waiter");
        assert_eq!(page[TX_GATE].load(Ordering::Relaxed), 0);
        assert!(server.shut(&page), "the shutdown asked of the departed owner");
    }

    /// A shutdown is the stream's: a write in another holder after it answers
    /// it and publishes nothing, and a second shutdown rings nobody.
    #[test]
    fn a_write_after_another_holders_shutdown_answers_shut_and_publishes_nothing() {
        let page = page();
        let mut server = Server::new(&page, LAYOUT);
        let ((_, tx), (_, other_tx)) = two(&page);
        let bells = quiet();
        shutdown(&page, &tx, &bells);
        assert_eq!(bells.rang.get(), 1);
        assert_eq!(write_one(&page, &other_tx, &bells, Mode::Nonblocking), (Err(Refused::Shut), 0));
        assert_eq!(server.sent(&page), Ok(0));
        shutdown(&page, &other_tx, &bells);
        assert_eq!(bells.rang.get(), 1, "a second shutdown rang");
    }

    /// A shutdown while another holder has the gate is that holder's to store
    /// as it lets go, and the shutdown returns once it did: the write the
    /// owner was making is published before it. Over a full send ring the
    /// shut alone is what `Server::rung` wakes the shutting holder for.
    #[test]
    fn a_shutdown_under_another_holders_gate_is_stored_by_its_release() {
        let page = page();
        let ((_, tx), _) = two(&page);
        let mut server = Server::new(&page, LAYOUT);
        // A full send ring: no room rings the shutting holder, only the shut.
        let filled = write(&page, &tx, &quiet(), Mode::Nonblocking, C, |_, _| {});
        assert_eq!(filled, Ok(C));
        let owner = Common { gate: TX_GATE, shut: Some(TX_END), gone: 13, holder: second() };
        assert_eq!(acquire(&page, owner), Ok(()));
        let released = Cell::new(false);
        let bells = Quiet {
            rang: Cell::new(0),
            gone: false,
            waited: Some(|| {
                assert_eq!(page[TX_GATE].load(Ordering::Relaxed), 2 | WANT | SHUT, "the owner was asked");
                assert_eq!(page[TX_ROOM].load(Ordering::Relaxed), 1, "the want armed before the gate's");
                assert!(release(&page, owner), "the owner rings for the shutdown");
                released.set(true);
            }),
            gates: RefCell::new(Vec::new()),
        };
        shutdown(&page, &tx, &bells);
        assert!(released.get(), "the shutdown returned before the owner let go");
        assert_eq!(*bells.gates.borrow(), [Some(2)], "the shutdown's wait is for the second holder's gate");
        assert_eq!(page[TX_END].load(Ordering::Relaxed), ORDERLY);
        assert_eq!(page[TX_GATE].load(Ordering::Relaxed), 0);
        assert_eq!(server.rung(&page), Ok(Rings { receive: false, send: true }), "the shut rings the holder asleep over a full ring");
    }

    /// A nonblocking write that finds room under another holder's gate arms
    /// its want, then the gate's, and answers `WouldBlock`, so the owner's
    /// release rings the server for it.
    #[test]
    fn a_write_under_another_holders_gate_arms_both_wants_and_would_block() {
        let page = page();
        let ((_, tx), _) = two(&page);
        page[TX_GATE].store(2, Ordering::Relaxed);
        let bells = quiet();
        assert_eq!(write_one(&page, &tx, &bells, Mode::Nonblocking), (Err(Refused::WouldBlock), 0));
        assert_eq!(page[TX_GATE].load(Ordering::Relaxed), 2 | WANT);
        assert_eq!(page[TX_ROOM].load(Ordering::Relaxed), 1);
    }

    /// A blocking write under another holder's gate waits as a wait for that
    /// holder's gate, which the adapter reports where it lasts, and the
    /// write goes on once the gate is let go; a write that waits for room
    /// waits for no gate.
    #[test]
    fn a_blocking_write_under_another_holders_gate_waits_for_that_holders_gate() {
        let page = page();
        let ((_, tx), _) = two(&page);
        let server = RefCell::new(Server::new(&page, LAYOUT));
        let owner = Common { gate: TX_GATE, shut: Some(TX_END), gone: 13, holder: second() };
        assert_eq!(acquire(&page, owner), Ok(()));
        let gated = Quiet {
            rang: Cell::new(0),
            gone: false,
            waited: Some(|| {
                let _ = release(&page, owner);
            }),
            gates: RefCell::new(Vec::new()),
        };
        assert_eq!(write_one(&page, &tx, &gated, Mode::Blocking(None)), (Ok(1), 1));
        assert_eq!(*gated.gates.borrow(), [Some(2)]);
        assert_eq!(write(&page, &tx, &quiet(), Mode::Nonblocking, C, |_, _| {}), Ok(C - 1), "the ring filled");
        let full = Quiet {
            rang: Cell::new(0),
            gone: false,
            waited: Some(|| {
                let _ = server.borrow_mut().release(&page, 1);
            }),
            gates: RefCell::new(Vec::new()),
        };
        assert_eq!(write_one(&page, &tx, &full, Mode::Blocking(None)), (Ok(1), 1));
        assert_eq!(*full.gates.borrow(), [None], "a wait for room");
    }

    /// A poll of a direction ready under another holder's gate watches for
    /// that holder's gate, which its waiter reports where it lasts; one that
    /// is not ready watches for no gate.
    #[test]
    fn a_watch_under_another_holders_gate_names_that_holder() {
        let page = page();
        let ((_, tx), _) = two(&page);
        let bells = quiet();
        let owner = Common { gate: TX_GATE, shut: Some(TX_END), gone: 13, holder: second() };
        assert_eq!(acquire(&page, owner), Ok(()));
        let Polled::Watching(gated) = watch(&page, &tx, &bells) else { panic!("ready under another's gate") };
        assert_eq!(gated.gate(), Some(2));
        let _ = unwatch(&page, &tx, &bells, gated);
        let _ = release(&page, owner);
        assert_eq!(write(&page, &tx, &bells, Mode::Nonblocking, C, |_, _| {}), Ok(C), "the ring filled");
        let Polled::Watching(full) = watch(&page, &tx, &bells) else { panic!("room in a full ring") };
        assert_eq!(full.gate(), None, "a watch for room");
        let _ = unwatch(&page, &tx, &bells, full);
    }

    /// Once one holder saw the server hang up, a write with room in another
    /// answers it and copies nothing, and a read answers it after the bytes.
    #[test]
    fn a_hang_up_one_holder_saw_is_every_holders() {
        let page = page();
        let mut server = Server::new(&page, LAYOUT);
        let ((rx, _), (other_rx, other_tx)) = two(&page);
        let _ = server.publish(&page, 1);
        let hung = Quiet::<fn()> { rang: Cell::new(0), gone: true, waited: None, gates: RefCell::new(Vec::new()) };
        assert_eq!(read(&page, &rx, &hung, Mode::Nonblocking, 1, |_, _| {}), Ok(1), "the byte before the hang-up");
        assert_eq!(read(&page, &rx, &hung, Mode::Nonblocking, 1, |_, _| {}), Err(Refused::Gone));
        let quiet = quiet();
        assert_eq!(write_one(&page, &other_tx, &quiet, Mode::Nonblocking), (Err(Refused::Gone), 0));
        assert_eq!(read(&page, &other_rx, &quiet, Mode::Nonblocking, 1, |_, _| {}), Err(Refused::Gone));
        assert_eq!(quiet.rang.get(), 0, "nothing was rung");
    }

    /// A ring that finds the server gone says so to every holder.
    #[test]
    fn a_ring_that_finds_the_server_gone_is_every_holders() {
        let page = page();
        let ((_, tx), (_, other_tx)) = two(&page);
        page[TX_DATA].store(1, Ordering::Relaxed);
        assert_eq!(write_one(&page, &tx, &HungRing, Mode::Nonblocking), (Ok(1), 1), "a write that rings the server");
        let quiet = quiet();
        assert_eq!(write_one(&page, &other_tx, &quiet, Mode::Nonblocking), (Err(Refused::Gone), 0));
    }

    /// Bells that hold nothing, whose ring finds the server gone.
    struct HungRing;

    impl Bells for HungRing {
        type Until = ();
        fn drain(&self, _: Doorbell) -> Took {
            Took::Nothing
        }
        fn wait(&self, _: Doorbell, _: Option<()>) -> Waited {
            unreachable!("a nonblocking call waited")
        }
        fn wait_gate(&self, _: Doorbell, _: Option<()>, _: u16) -> Waited {
            unreachable!("a nonblocking call waited")
        }
        fn ring(&self) -> Result<(), HungUp> {
            Err(HungUp)
        }
    }

    /// A receiving half one holder shut down reads nothing in another, its
    /// bytes left, and the server rings a reader asleep for it.
    #[test]
    fn a_read_shutdown_is_every_holders() {
        let page = page();
        let mut server = Server::new(&page, LAYOUT);
        let ((rx, _), (other_rx, _)) = two(&page);
        let bells = quiet();
        assert_eq!(read(&page, &other_rx, &bells, Mode::Nonblocking, 1, |_, _| {}), Err(Refused::WouldBlock), "armed");
        shutdown_read(&page, &rx, &bells);
        assert_eq!(bells.rang.get(), 1);
        assert_eq!(server.rung(&page), Ok(Rings { receive: true, send: false }), "the reader that armed");
        let _ = server.publish(&page, 2);
        assert_eq!(read(&page, &other_rx, &bells, Mode::Nonblocking, 1, |_, _| {}), Ok(0));
        assert_eq!(server.held(&page), Ok(2), "a byte was read after the shutdown");
    }

    /// A copy that unwinds out of the section lets the gate go and rings the
    /// server, whose want it may have left unrung.
    #[test]
    fn a_copy_that_unwinds_lets_the_gate_go_and_rings() {
        let page = page();
        let (_, tx) = client(&page, LAYOUT);
        let (tx, bells) = (Held(RefCell::new(tx)), quiet());
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            write(&page, &tx, &bells, Mode::Nonblocking, 1, |_, _| panic!("the copy faulted"))
        }));
        assert!(unwound.is_err());
        assert_eq!(page[TX_GATE].load(Ordering::Relaxed), 0, "the gate is still held");
        assert_eq!(bells.rang.get(), 1, "the server was not rung");
        assert_eq!(write_one(&page, &tx, &bells, Mode::Nonblocking), (Ok(1), 1), "the next write");
    }
}
