//! One connection's requests in flight, up to [`SLOTS`] at once. A request
//! holds a slot, which is its span of the window and where its reply is
//! filed, and a tag naming the slot; one too large for a slot holds them all.
//!
//! **Whoever waits receives.** A caller waiting for its reply takes the
//! receive turn if nobody holds it, and takes replies off its connection,
//! filing each other caller's in that caller's slot, until its own comes; then
//! it wakes a caller still waiting, which takes the turn if it is still free.
//! A receive that fails, or a reply naming no request in flight, fails every
//! request in flight on that connection.
//!
//! **One lock.** Every slot's request and the turn are read and changed only
//! under the table's lock, which is held for nothing else; a caller sleeps on
//! its slot's own word, which whoever changes its request moves after.
//!
//! Built on `flight_prim`, which `kernel-loom` swaps for loom's lock and a
//! futex that wakes only the word woken, so its models run this file.

use super::flight_prim::{wait, wake_all, wake_one, AtomicU32, Lock, Ordering};

/// The requests one connection has in flight at most.
pub const SLOTS: usize = 16;

/// Every slot free.
const ALL: u32 = (1 << SLOTS) - 1;

/// Requests in flight, answered with `R` or failed with `E`.
pub struct Flight<R, E> {
    table: Lock<Table<R, E>>,
    /// Moved whenever slots are given back; a caller waiting for one sleeps
    /// on it.
    freed: AtomicU32,
    /// Moved whenever a slot's request is answered or failed, or its caller
    /// is asked to take the turn; that caller sleeps on it.
    woken: [AtomicU32; SLOTS],
}

struct Table<R, E> {
    /// One bit for each slot nobody holds.
    free: u32,
    /// Callers waiting for every slot, ahead of whom no slot is taken.
    wholes: u32,
    /// A caller holds the receive turn.
    receiving: bool,
    /// Requests sent, which a tag counts above its slot.
    sent: u64,
    requests: [Request<R, E>; SLOTS],
}

struct Request<R, E> {
    /// The connection it went out on, by generation.
    link: u64,
    tag: u64,
    state: State<R, E>,
}

enum State<R, E> {
    /// Not in flight.
    Idle,
    /// Sent, or about to be, its reply not yet filed.
    Waiting,
    Filed(Result<R, E>),
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
            table: Lock::new(Table {
                free: ALL,
                wholes: 0,
                receiving: false,
                sent: 0,
                requests: core::array::from_fn(|_| Request { link: 0, tag: 0, state: State::Idle }),
            }),
            freed: AtomicU32::new(0),
            woken: core::array::from_fn(|_| AtomicU32::new(0)),
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
        let mut table = self.table.lock();
        let tag = (table.sent << SLOTS.trailing_zeros()) | taken.first as u64;
        table.sent += 1;
        table.requests[taken.first] = Request { link, tag, state: State::Waiting };
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
        let mine = taken.first;
        loop {
            let seen = self.woken[mine].load(Ordering::Acquire);
            {
                let mut table = self.table.lock();
                if let Some(filed) = table.take_filed(mine) {
                    return filed;
                }
                if !table.receiving {
                    table.receiving = true;
                    let link = table.requests[mine].link;
                    drop(table);
                    return self.lead(mine, link, &mut receive, malformed);
                }
            }
            wait(&self.woken[mine], seen);
        }
    }

    /// Receive on connection `link`, filing every reply, until slot `mine`'s
    /// is filed; then let the turn go to a caller still waiting.
    fn lead(&self, mine: usize, link: u64, receive: &mut impl FnMut() -> Result<(u64, R), E>, malformed: E) -> Result<R, E>
    where
        E: Copy,
    {
        loop {
            let got = receive();
            let mut table = self.table.lock();
            let woken = match got {
                Ok((tag, reply)) => table.file(link, tag, reply).unwrap_or_else(|_| table.fail(link, malformed)),
                Err(why) => table.fail(link, why),
            };
            let answered = table.take_filed(mine);
            let next = answered.is_some().then(|| {
                table.receiving = false;
                table.waiting(Some(link))
            });
            drop(table);
            self.wake(woken & !(1 << mine));
            if let Some(answered) = answered {
                self.wake(next.flatten().map_or(0, |i| 1 << i));
                return answered;
            }
        }
    }

    /// Give `taken` back, answered or never sent. A request never sent, whose
    /// caller may have been woken to take the turn, wakes another waiting
    /// caller while nobody receives.
    pub fn give(&self, taken: Taken) {
        let next = {
            let mut table = self.table.lock();
            let unsent = matches!(table.requests[taken.first].state, State::Waiting);
            table.requests[taken.first].state = State::Idle;
            table.free |= if taken.count == SLOTS { ALL } else { 1 << taken.first };
            if unsent && !table.receiving { table.waiting(None) } else { None }
        };
        self.freed.fetch_add(1, Ordering::Release);
        wake_all(&self.freed);
        self.wake(next.map_or(0, |i| 1 << i));
    }

    /// Wake the callers of the slots `slots` names.
    fn wake(&self, slots: u32) {
        for i in (0..SLOTS).filter(|i| slots & (1 << i) != 0) {
            self.woken[i].fetch_add(1, Ordering::Release);
            wake_one(&self.woken[i]);
        }
    }
}

impl<R, E> Table<R, E> {
    /// File `reply` where it waits on connection `link`: the slot to wake,
    /// or `Err` for a tag naming no request waiting there.
    fn file(&mut self, link: u64, tag: u64, reply: R) -> Result<u32, ()> {
        let i = (tag % SLOTS as u64) as usize;
        let request = &mut self.requests[i];
        if !matches!(request.state, State::Waiting) || request.tag != tag || request.link != link {
            return Err(());
        }
        request.state = State::Filed(Ok(reply));
        Ok(1 << i)
    }

    /// Fail every request waiting on connection `link` with `why`: the slots
    /// to wake.
    fn fail(&mut self, link: u64, why: E) -> u32
    where
        E: Copy,
    {
        let mut failed = 0;
        for (i, request) in self.requests.iter_mut().enumerate() {
            if matches!(request.state, State::Waiting) && request.link == link {
                request.state = State::Filed(Err(why));
                failed |= 1 << i;
            }
        }
        failed
    }

    /// What was filed in slot `i`, taken by its caller.
    fn take_filed(&mut self, i: usize) -> Option<Result<R, E>> {
        match core::mem::replace(&mut self.requests[i].state, State::Idle) {
            State::Filed(filed) => Some(filed),
            other => {
                self.requests[i].state = other;
                None
            }
        }
    }

    /// A slot whose caller still waits, one on connection `link` first: the
    /// caller to take the turn next.
    fn waiting(&self, link: Option<u64>) -> Option<usize> {
        let waits = |i: &usize| matches!(self.requests[*i].state, State::Waiting);
        let on_link = (0..SLOTS).filter(waits).find(|&i| Some(self.requests[i].link) == link);
        on_link.or_else(|| (0..SLOTS).find(waits))
    }
}
