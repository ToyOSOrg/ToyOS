//! One connection's requests in flight, up to [`SLOTS`] at once. A request
//! holds a slot, which is its span of the window and where its reply is
//! filed, and a tag naming the slot; one too large for a slot holds them all.
//!
//! **Whoever waits receives.** A caller waiting for its reply takes the
//! receive turn if it is free, and takes replies off its connection, filing
//! each other caller's in that caller's slot, until its own comes; then it
//! hands the turn to a caller still waiting. A receive that fails, or a reply
//! naming no request in flight, fails every request in flight on that
//! connection.
//!
//! Built on `flight_prim`, which `kernel-loom` swaps for loom's atomics, cells
//! and lock, so its model runs this file.

use super::flight_prim::{wait, wake_all, wake_one, AtomicU32, AtomicU64, Cell, Lock, Ordering};

/// The requests one connection has in flight at most.
pub const SLOTS: usize = 16;

/// Every slot free.
const ALL: u32 = (1 << SLOTS) - 1;

/// A slot's request: not in flight.
const IDLE: u32 = 0;
/// Sent, its reply not yet filed.
const WAITING: u32 = 1;
/// Waiting, and handed the receive turn.
const LEAD: u32 = 2;
/// Its reply is filed.
const ANSWERED: u32 = 3;
/// Its connection failed first: the failure is filed.
const FAILED: u32 = 4;

/// Requests in flight, answered with `R` or failed with `E`.
pub struct Flight<R, E> {
    table: Lock<Table>,
    /// Moved whenever slots are given back; a caller waiting for one sleeps
    /// on it.
    freed: AtomicU32,
    /// 1 while a caller holds the receive turn.
    turn: AtomicU32,
    /// Requests sent, which a tag counts above its slot.
    sent: AtomicU64,
    slots: [Slot<R, E>; SLOTS],
}

// SAFETY: a slot's cell is written only by the holder of the receive turn,
// while its state says the request waits, and read only by the request's
// caller once its state says it was filed, after the store that says so.
unsafe impl<R: Send, E: Send> Sync for Flight<R, E> {}

struct Table {
    /// One bit for each slot nobody holds.
    free: u32,
    /// Callers waiting for every slot, ahead of whom no slot is taken.
    wholes: u32,
}

struct Slot<R, E> {
    state: AtomicU32,
    tag: AtomicU64,
    /// The connection its request went out on, by generation.
    link: AtomicU64,
    filed: Cell<Option<Result<R, E>>>,
}

/// The slots one caller holds, from `first`: one, or all of them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Taken {
    first: usize,
    count: usize,
}

impl Taken {
    pub fn first(self) -> usize {
        self.first
    }

    pub fn count(self) -> usize {
        self.count
    }
}

impl<R, E> Default for Flight<R, E> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R, E> Flight<R, E> {
    pub fn new() -> Self {
        Self {
            table: Lock::new(Table { free: ALL, wholes: 0 }),
            freed: AtomicU32::new(0),
            turn: AtomicU32::new(0),
            sent: AtomicU64::new(0),
            slots: core::array::from_fn(|_| Slot {
                state: AtomicU32::new(IDLE),
                tag: AtomicU64::new(0),
                link: AtomicU64::new(0),
                filed: Cell::new(None),
            }),
        }
    }

    /// A slot, or every slot where `whole`, once they are free. A caller
    /// waiting for every slot goes before any that asks for one after it.
    pub fn take(&self, whole: bool) -> Taken {
        let mut counted = false;
        loop {
            let seen = self.freed.load(Ordering::Acquire);
            {
                let mut table = self.table.lock();
                if whole {
                    if table.free == ALL {
                        table.free = 0;
                        table.wholes -= u32::from(counted);
                        return Taken { first: 0, count: SLOTS };
                    }
                    table.wholes += u32::from(!counted);
                    counted = true;
                } else if table.wholes == 0 && table.free != 0 {
                    let first = table.free.trailing_zeros() as usize;
                    table.free &= !(1 << first);
                    return Taken { first, count: 1 };
                }
            }
            wait(&self.freed, seen);
        }
    }

    /// The request on `taken` goes out on the connection of generation
    /// `link`: the tag it carries. Called before the send, so its reply always
    /// finds it waiting.
    pub fn sent(&self, taken: Taken, link: u64) -> u64 {
        let tag = (self.sent.fetch_add(1, Ordering::Relaxed) << SLOTS.trailing_zeros()) | taken.first as u64;
        let slot = &self.slots[taken.first];
        slot.link.store(link, Ordering::Relaxed);
        slot.tag.store(tag, Ordering::Relaxed);
        slot.state.store(WAITING, Ordering::Release);
        tag
    }

    /// The reply to the request on `taken`, or the failure of its
    /// connection. While nobody else does, this caller receives, with
    /// `receive`, which waits for the next reply on that connection and
    /// answers its tag; a tag naming no request in flight there fails them
    /// all with `malformed`.
    pub fn answer(&self, taken: Taken, mut receive: impl FnMut() -> Result<(u64, R), E>, malformed: E) -> Result<R, E>
    where
        E: Copy,
    {
        let mine = &self.slots[taken.first];
        loop {
            match mine.state.load(Ordering::Acquire) {
                ANSWERED | FAILED => return mine.take_filed(),
                // Handed the turn: take it below, or sleep again where
                // another caller took it first.
                LEAD => {
                    let _ = mine.state.compare_exchange(LEAD, WAITING, Ordering::Relaxed, Ordering::Relaxed);
                }
                _ => {}
            }
            if self.turn.compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed).is_ok() {
                let answered = self.lead(taken.first, &mut receive, malformed);
                self.turn.store(0, Ordering::Release);
                self.hand_on();
                return answered;
            }
            wait(&mine.state, WAITING);
        }
    }

    /// Receive on the connection slot `mine` waits on, filing every reply,
    /// until its own is filed.
    fn lead(&self, mine: usize, receive: &mut impl FnMut() -> Result<(u64, R), E>, malformed: E) -> Result<R, E>
    where
        E: Copy,
    {
        let link = self.slots[mine].link.load(Ordering::Relaxed);
        loop {
            if let ANSWERED | FAILED = self.slots[mine].state.load(Ordering::Acquire) {
                return self.slots[mine].take_filed();
            }
            match receive() {
                Ok((tag, reply)) => {
                    let to = &self.slots[(tag % SLOTS as u64) as usize];
                    let waits = matches!(to.state.load(Ordering::Acquire), WAITING | LEAD);
                    if waits && to.tag.load(Ordering::Relaxed) == tag && to.link.load(Ordering::Relaxed) == link {
                        to.file(Ok(reply), ANSWERED);
                    } else {
                        self.fail(link, malformed);
                    }
                }
                Err(why) => self.fail(link, why),
            }
        }
    }

    /// Every request waiting on the connection of generation `link` fails
    /// with `why`.
    fn fail(&self, link: u64, why: E)
    where
        E: Copy,
    {
        for slot in &self.slots {
            if matches!(slot.state.load(Ordering::Acquire), WAITING | LEAD) && slot.link.load(Ordering::Relaxed) == link {
                slot.file(Err(why), FAILED);
            }
        }
    }

    /// Hand the receive turn, just let go, to one caller still waiting.
    fn hand_on(&self) {
        for slot in &self.slots {
            if slot.state.compare_exchange(WAITING, LEAD, Ordering::AcqRel, Ordering::Relaxed).is_ok() {
                wake_one(&slot.state);
                return;
            }
        }
    }

    /// Give `taken` back, answered or never sent. A turn it was handed and
    /// never took goes on to another caller.
    pub fn give(&self, taken: Taken) {
        let handed = self.slots[taken.first].state.swap(IDLE, Ordering::AcqRel) == LEAD;
        {
            let mut table = self.table.lock();
            let bits = if taken.count == SLOTS { ALL } else { 1 << taken.first };
            table.free |= bits;
        }
        self.freed.fetch_add(1, Ordering::Release);
        wake_all(&self.freed);
        if handed {
            self.hand_on();
        }
    }
}

impl<R, E> Slot<R, E> {
    /// File `filed`, and say so with `state`: a swap, as every change of a
    /// waiting slot's state is a read-modify-write, so a turn handed to it
    /// meanwhile is ordered before the filing and never after.
    fn file(&self, filed: Result<R, E>, state: u32) {
        // SAFETY: the receive turn's holder alone writes a waiting slot's
        // cell, and its caller reads it only after the swap below.
        self.filed.with_mut(|cell| unsafe { *cell = Some(filed) });
        self.state.swap(state, Ordering::Release);
        wake_one(&self.state);
    }

    /// What was filed, taken by the request's own caller.
    fn take_filed(&self) -> Result<R, E> {
        // SAFETY: the state said it was filed, read with `Acquire`, and
        // nothing files a slot that no longer waits.
        self.filed.with_mut(|cell| unsafe { (*cell).take() }).expect("flight: a slot said filed holds its reply")
    }
}
