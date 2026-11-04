//! Journal-owned keys no node holds yet: a replay's, reclaim's own updates,
//! and an ordinary change to a node that could not take it then. After a
//! replay it can hold a whole journal, so an operation costs a seek per run
//! and, amortized, the entries it returns or changes, never a pass over the
//! whole set.

use alloc::borrow::Cow;
use alloc::collections::VecDeque;
use alloc::vec::Vec;
#[cfg(test)]
use core::cell::Cell;
use core::iter::Peekable;
use core::slice;

use super::bkey::{Bpos, Key};
use super::{Btree, Error, Result};
use super::{journal, kind};

pub(crate) type Position = (Btree, u8, Bpos);
pub(crate) type Update = (Btree, u8, Key, Vec<u8>);
const LEVELS: usize = u64::BITS as usize;

struct Entry {
    btree: Btree,
    level: u8,
    key: Key,
    value: Vec<u8>,
    first: u64,
    last: u64,
    order: u64,
    live: bool,
    /// Its index once its run is compacted.
    slot: usize,
}

impl Entry {
    fn position(&self) -> Position { (self.btree, self.level, self.key.pos) }
    fn ordering(&self) -> (Position, u64) { (self.position(), self.order) }
    fn accounting(&self) -> bool { self.btree == Btree::Accounting && self.level == 0 }
}

struct Run {
    entries: Vec<Entry>,
    /// Each entry's pin as it stood when the run was made, oldest first. A
    /// pin `durable` later moved is the lifted queue's.
    pins: Vec<(u64, usize)>,
    oldest: usize,
    dead: usize,
}

// Entries the calling thread's pending operations have looked at: what the
// tests that pin each operation's cost count.
#[cfg(test)]
std::thread_local! { static VISITED: Cell<usize> = const { Cell::new(0) }; }

#[cfg(test)]
fn visit() { VISITED.with(|v| v.set(v.get() + 1)); }
#[cfg(not(test))]
fn visit() {}

#[cfg(test)]
fn visited() -> usize { VISITED.with(Cell::get) }

/// What `f` returns, and the entries pending looked at meanwhile.
#[cfg(test)]
fn visits<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let before = visited();
    let out = f();
    (out, visited() - before)
}

impl Run {
    fn pinned(entries: &[Entry], &(first, i): &(u64, usize)) -> bool {
        entries[i].live && entries[i].first == first
    }

    fn oldest(&self) -> Option<&Entry> {
        self.pins.get(self.oldest).map(|&(_, i)| &self.entries[i])
    }

    fn retire(&mut self) {
        while self.pins.get(self.oldest).is_some_and(|pin| !Self::pinned(&self.entries, pin)) { self.oldest += 1; }
    }

    /// The entries at `position`.
    fn at(&self, position: Position) -> core::ops::Range<usize> {
        let from = self.entries.partition_point(|e| e.position() < position);
        from..from + self.entries[from..].partition_point(|e| e.position() == position)
    }

    fn find(&self, wanted: (Position, u64)) -> Option<usize> {
        let at = self.entries.partition_point(|e| e.ordering() < wanted);
        self.entries.get(at).is_some_and(|e| e.live && e.ordering() == wanted).then_some(at)
    }

    fn kill(&mut self, i: usize, live: &mut Live) {
        let entry = &mut self.entries[i];
        entry.live = false;
        entry.value = Vec::new();
        live.remove(entry);
        self.dead += 1;
    }

    /// Drops the dead entries once they are half the run, in place, so a
    /// range never walks more dead entries than live ones it passes.
    fn compact(&mut self) {
        if self.dead * 2 <= self.entries.len() { return; }
        for (slot, entry) in self.entries.iter_mut().filter(|e| e.live).enumerate() {
            entry.slot = slot;
        }
        let entries = &self.entries;
        self.pins.retain(|pin| Self::pinned(entries, pin));
        for pin in &mut self.pins { pin.1 = entries[pin.1].slot; }
        self.entries.retain(|e| e.live);
        self.dead = 0;
        self.oldest = 0;
    }
}

/// Live entries of each btree, its leaves apart from its interior levels:
/// where none are, a lookup reads the node alone.
struct Live([[usize; 2]; Btree::ALL.len()]);

impl Live {
    fn of(&mut self, entry: &Entry) -> &mut usize { &mut self.0[entry.btree as usize][usize::from(entry.level > 0)] }
    fn add(&mut self, entry: &Entry) { *self.of(entry) += 1; }
    fn remove(&mut self, entry: &Entry) { *self.of(entry) -= 1; }
    fn any(&self, btree: Btree, level: u8) -> bool { self.0[btree as usize][usize::from(level > 0)] > 0 }
}

/// An identity survives run merges; a newer update at its position does not
/// become part of a transfer already captured.
#[derive(Clone, Copy)]
pub(crate) struct Capture {
    pub position: Position,
    pub order: u64,
    pub first: u64,
}

pub(crate) struct View<'a> {
    entry: &'a Entry,
    first: u64,
}

impl<'a> View<'a> {
    pub fn position(&self) -> Position { self.entry.position() }
    pub fn key(&self) -> &'a Key { &self.entry.key }
    pub fn value(&self) -> &'a [u8] { &self.entry.value }
    pub fn capture(&self) -> Capture { Capture { position: self.position(), order: self.entry.order, first: self.first } }
}

pub(crate) struct Transferred {
    pub key: Key,
    pub value: Vec<u8>,
    pub first: u64,
    pub last: u64,
}

pub(crate) struct Pending {
    runs: [Option<Run>; LEVELS],
    incoming: Vec<Entry>,
    merged: Vec<Entry>,
    pins: Vec<(u64, usize)>,
    next: u64,
    prepared: usize,
    /// Ordinary updates in sequence order, until `durable` reaches theirs.
    undurable: VecDeque<(u64, Position)>,
    /// Entries `durable` moved to their own sequence's pin, in that order.
    lifted: VecDeque<(u64, Position, u64)>,
    live: Live,
}

impl Default for Pending {
    fn default() -> Self {
        Self {
            runs: core::array::from_fn(|_| None),
            incoming: Vec::new(),
            merged: Vec::new(),
            pins: Vec::new(),
            next: 0,
            prepared: 0,
            undurable: VecDeque::new(),
            lifted: VecDeque::new(),
            live: Live([[0; 2]; Btree::ALL.len()]),
        }
    }
}

impl Pending {
    /// Reserve all workspace before making a logical batch visible.
    pub fn reserve(&mut self, additional: usize) -> Result<()> {
        #[cfg(test)]
        let _allocations = tests::allocation::Scope::enter();
        assert!(self.incoming.is_empty() && self.merged.is_empty());
        let mut total = additional;
        for run in &self.runs {
            let Some(run) = run else { break };
            total = total.checked_add(run.entries.len() - run.dead).ok_or(Error::NoMemory)?;
        }
        if self.runs.iter().all(Option::is_some) || u64::try_from(additional).ok().and_then(|n| self.next.checked_add(n)).is_none() {
            return Err(Error::NoMemory);
        }
        // Each update `durable` releases lifts at most one entry.
        let lifts = self.undurable.len().checked_add(additional).ok_or(Error::NoMemory)?;
        self.incoming.try_reserve(additional).map_err(|_| Error::NoMemory)?;
        self.merged.try_reserve(total).map_err(|_| Error::NoMemory)?;
        self.pins.try_reserve(total).map_err(|_| Error::NoMemory)?;
        self.undurable.try_reserve(additional).map_err(|_| Error::NoMemory)?;
        self.lifted.try_reserve(lifts).map_err(|_| Error::NoMemory)?;
        self.prepared = additional;
        Ok(())
    }

    /// `reserve` funded this batch. Values move; sorting and merging allocate
    /// nothing. Equal positions retain the latest value and earliest pin.
    pub fn apply(&mut self, seq: u64, updates: impl IntoIterator<Item = Update>) {
        for (btree, level, key, value) in updates {
            assert!(self.incoming.len() < self.prepared, "pending batch exceeded its reservation");
            let entry = Entry { btree, level, key, value, first: seq, last: seq, order: self.next, live: true, slot: 0 };
            if !entry.accounting() { self.undurable.push_back((seq, entry.position())); }
            self.live.add(&entry);
            self.incoming.push(entry);
            self.next += 1;
        }
        self.prepared = 0;
        if self.incoming.is_empty() { return; }
        self.incoming.sort_unstable_by_key(Entry::ordering);
        let level = self.runs.iter().position(Option::is_none).expect("a prepared run slot");
        let mut at = [0usize; LEVELS];
        let mut incoming_at = 0;
        loop {
            let mut chosen = None;
            let mut best = self.incoming.get(incoming_at).map(Entry::ordering);
            for (i, cursor) in at.iter_mut().enumerate().take(level) {
                let run = &self.runs[i].as_ref().expect("preceding run occupied").entries;
                while run.get(*cursor).is_some_and(|e| !e.live) { *cursor += 1; }
                if let Some(e) = run.get(*cursor) {
                    if best.is_none_or(|old| e.ordering() < old) {
                        best = Some(e.ordering());
                        chosen = Some(i);
                    }
                }
            }
            if best.is_none() { break; }
            let source = match chosen {
                Some(i) => {
                    let e = &mut self.runs[i].as_mut().unwrap().entries[at[i]];
                    at[i] += 1;
                    e
                }
                None => {
                    let e = &mut self.incoming[incoming_at];
                    incoming_at += 1;
                    e
                }
            };
            let mut entry = Entry { value: core::mem::take(&mut source.value), ..*source };
            source.live = false;
            match self.merged.last_mut() {
                Some(old) if old.position() == entry.position() && !entry.accounting() => {
                    entry.first = entry.first.min(old.first);
                    self.live.remove(old);
                    *old = entry;
                }
                _ => self.merged.push(entry),
            }
        }
        self.incoming.clear();
        for run in &mut self.runs[..level] { *run = None; }
        let merged = &self.merged;
        self.pins.extend(merged.iter().enumerate().map(|(i, e)| (e.first, i)));
        self.pins.sort_unstable_by_key(|&(first, i)| (first, merged[i].order));
        self.runs[level] = Some(Run { entries: core::mem::take(&mut self.merged), pins: core::mem::take(&mut self.pins), oldest: 0, dead: 0 });
        self.settle();
    }

    pub fn oldest_pin(&self) -> Option<u64> {
        self.oldest().map(|c| c.first)
    }

    pub fn oldest(&self) -> Option<Capture> {
        let lifted = self.lifted.front().and_then(|&(first, position, order)| {
            self.entry((position, order)).filter(|e| e.first == first)
        });
        self.runs.iter().flatten().filter_map(Run::oldest).chain(lifted).min_by_key(|e| (e.first, e.order))
            .map(|e| Capture { position: e.position(), order: e.order, first: e.first })
    }

    fn entry(&self, wanted: (Position, u64)) -> Option<&Entry> {
        self.runs.iter().flatten().find_map(|run| run.find(wanted).map(|i| &run.entries[i]))
    }

    /// Drops lifted pins whose entries moved on, so `oldest` finds a live one
    /// first.
    fn settle(&mut self) {
        while let Some(&(first, position, order)) = self.lifted.front() {
            if self.entry((position, order)).is_some_and(|e| e.first == first) { break; }
            self.lifted.pop_front();
        }
    }

    /// A durable ordinary replacement no longer needs the replaced record.
    /// Captures retain their original dependency; accounting remains additive.
    /// Only positions updated since the last call can change.
    pub fn durable(&mut self, seq: u64) {
        while let Some(&(at, position)) = self.undurable.front() {
            if at > seq { break; }
            self.undurable.pop_front();
            self.release(position, seq);
        }
        for run in self.runs.iter_mut().flatten() { run.retire(); }
        self.settle();
    }

    fn release(&mut self, position: Position, seq: u64) {
        let replacement = self.runs.iter().flatten()
            .flat_map(|run| run.entries[run.at(position)].iter())
            .inspect(|_| visit())
            .filter(|e| e.live && !e.accounting() && e.last <= seq)
            .map(|e| e.order)
            .max();
        let Some(newer) = replacement else { return };
        for slot in &mut self.runs {
            let Some(run) = slot else { continue };
            for i in run.at(position) {
                let entry = &mut run.entries[i];
                if !entry.live || entry.accounting() { continue; }
                if entry.order < newer {
                    run.kill(i, &mut self.live);
                } else if entry.order == newer && entry.first != entry.last {
                    entry.first = entry.last;
                    assert!(self.lifted.len() < self.lifted.capacity(), "a lifted pin past its reservation");
                    self.lifted.push_back((entry.last, position, entry.order));
                }
            }
            run.compact();
            if run.entries.is_empty() { *slot = None; }
        }
    }

    #[cfg(test)]
    pub fn allocated_bytes(&self) -> usize {
        let entries = |entries: &Vec<Entry>| entries.capacity() * core::mem::size_of::<Entry>() + entries.iter().map(|e| e.value.capacity()).sum::<usize>();
        let pins = |pins: &Vec<(u64, usize)>| pins.capacity() * core::mem::size_of::<(u64, usize)>();
        core::mem::size_of::<Self>() + entries(&self.incoming) + entries(&self.merged) + pins(&self.pins)
            + self.undurable.capacity() * core::mem::size_of::<(u64, Position)>() + self.lifted.capacity() * core::mem::size_of::<(u64, Position, u64)>()
            + self.runs.iter().flatten().map(|r| entries(&r.entries) + pins(&r.pins)).sum::<usize>()
    }

    #[cfg(test)]
    pub fn iter(&self) -> Iter<'_> { self.between(None) }

    #[cfg(test)]
    pub fn shrink(&mut self) {
        self.incoming.shrink_to_fit();
        self.merged.shrink_to_fit();
        self.pins.shrink_to_fit();
        self.undurable.shrink_to_fit();
        self.lifted.shrink_to_fit();
    }

    /// Whether a live entry lies in `[min, max]` of `btree` at `level`.
    pub fn holds(&self, btree: Btree, level: u8, min: Bpos, max: Bpos) -> bool {
        self.live.any(btree, level) && self.range(btree, level, min, max).next().is_some()
    }

    pub fn range(&self, btree: Btree, level: u8, min: Bpos, max: Bpos) -> Iter<'_> {
        self.between(Some(((btree, level, min), (btree, level, max))))
    }

    /// The logical range, preserving zero accounting values and their
    /// versions so materialization cannot apply an old delta twice.
    pub fn over(&self, btree: Btree, level: u8, min: Bpos, max: Bpos, keys: Vec<(Key, Vec<u8>)>) -> Result<Vec<(Key, Vec<u8>)>> {
        self.overlay(btree, level, min, max, &keys).map(|kv| kv.map(|(k, v)| (k, v.into_owned()))).collect()
    }

    /// `stored`, a node's keys in position order, with the pending keys of
    /// `[min, max]` laid over them from `min` on: [`Pending::over`] a key at
    /// a time.
    pub fn overlay<'a>(&'a self, btree: Btree, level: u8, min: Bpos, max: Bpos, stored: &'a [(Key, Vec<u8>)]) -> Overlay<'a> {
        let from = stored.partition_point(|(k, _)| k.pos < min);
        let to = stored.partition_point(|(k, _)| k.pos <= max).max(from);
        let entries = self.live.any(btree, level).then(|| self.range(btree, level, min, max).peekable());
        Overlay { pending: self, btree, level, stored: stored[from..to].iter().peekable(), entries }
    }

    fn between(&self, bounds: Option<(Position, Position)>) -> Iter<'_> {
        let mut at = [0; LEVELS];
        if let Some((min, _)) = bounds {
            for (i, run) in self.runs.iter().enumerate() {
                if let Some(run) = run { at[i] = run.entries.partition_point(|e| e.position() < min); }
            }
        }
        Iter { pending: self, at, end: bounds.map(|(_, max)| max), saved: None }
    }

    /// The callback installs the value and its pin before the source retires.
    /// It is infallible and cannot yield. `false` means capture was superseded.
    pub fn transfer(&mut self, capture: Capture, install: impl FnOnce(Transferred)) -> bool {
        let wanted = (capture.position, capture.order);
        let Some(source) = self.runs.iter_mut().flatten().find_map(|run| run.find(wanted).map(|i| &mut run.entries[i])) else { return false };
        let accounting = source.accounting();
        let value = core::mem::take(&mut source.value);
        install(Transferred { key: source.key, value, first: capture.first, last: source.last });
        for slot in &mut self.runs {
            let Some(run) = slot else { continue };
            for i in run.at(capture.position) {
                let entry = &run.entries[i];
                if entry.live && entry.order <= capture.order && (entry.order == capture.order || !accounting) { run.kill(i, &mut self.live); }
            }
            run.compact();
            run.retire();
            if run.entries.is_empty() { *slot = None; }
        }
        self.settle();
        true
    }
}

/// A node's keys with the pending keys of its range laid over them.
pub(crate) struct Overlay<'a> {
    pending: &'a Pending,
    btree: Btree,
    level: u8,
    stored: Peekable<slice::Iter<'a, (Key, Vec<u8>)>>,
    /// None where pending holds nothing of this btree and level.
    entries: Option<Peekable<Iter<'a>>>,
}

impl<'a> Overlay<'a> {
    /// An accounting key adds every pending delta newer than its version.
    fn accounted(&self, pos: Bpos, stored: Option<&(Key, Vec<u8>)>) -> Result<(Key, Cow<'a, [u8]>)> {
        let (mut key, mut sums) = match stored {
            Some((key, value)) => (*key, journal::counters(value)?),
            None => (Key::new(kind::ACCOUNTING, pos), Vec::new()),
        };
        let since = key.version;
        for entry in self.pending.range(self.btree, self.level, pos, pos) {
            let version = entry.key().version;
            if version <= since { continue; }
            let delta = journal::counters(entry.value())?;
            sums.resize(sums.len().max(delta.len()), 0);
            for (sum, d) in sums.iter_mut().zip(delta) { *sum = sum.wrapping_add(d); }
            key.version = key.version.max(version);
        }
        Ok((key, Cow::Owned(super::value::counters(&sums))))
    }
}

impl<'a> Iterator for Overlay<'a> {
    type Item = Result<(Key, Cow<'a, [u8]>)>;
    fn next(&mut self) -> Option<Self::Item> {
        let stored_next = |stored: &mut Peekable<slice::Iter<'a, (Key, Vec<u8>)>>| stored.next().map(|(k, v)| Ok((*k, Cow::Borrowed(&v[..]))));
        let Some(entries) = &mut self.entries else { return stored_next(&mut self.stored) };
        loop {
            let stored = self.stored.peek().map(|(k, _)| k.pos);
            let Some(pos) = entries.peek().map(|e| e.key().pos).filter(|p| stored.is_none_or(|s| *p <= s)) else {
                return stored_next(&mut self.stored);
            };
            let stored = self.stored.next_if(|(k, _)| k.pos == pos);
            if self.btree == Btree::Accounting && self.level == 0 {
                while entries.next_if(|e| e.key().pos == pos).is_some() {}
                return Some(self.accounted(pos, stored));
            }
            let latest = entries.next().unwrap();
            if latest.key().kind != kind::DELETED { return Some(Ok((*latest.key(), Cow::Borrowed(latest.value())))); }
        }
    }
}

pub(crate) struct Iter<'a> {
    pending: &'a Pending,
    at: [usize; LEVELS],
    end: Option<Position>,
    saved: Option<&'a Entry>,
}

impl<'a> Iter<'a> {
    fn raw(&mut self) -> Option<&'a Entry> {
        if let Some(saved) = self.saved.take() { return Some(saved); }
        let mut chosen = None;
        let mut best = None;
        for (i, run) in self.pending.runs.iter().enumerate() {
            let Some(Run { entries: run, .. }) = run else { continue };
            let within = |e: &Entry| self.end.is_none_or(|end| e.position() <= end);
            while run.get(self.at[i]).is_some_and(|e| !e.live && within(e)) {
                visit();
                self.at[i] += 1;
            }
            visit();
            if let Some(entry) = run.get(self.at[i]).filter(|e| within(e)) {
                if best.is_none_or(|old| entry.ordering() < old) {
                    chosen = Some(i);
                    best = Some(entry.ordering());
                }
            }
        }
        let i = chosen?;
        let entry = &self.pending.runs[i].as_ref().unwrap().entries[self.at[i]];
        self.at[i] += 1;
        Some(entry)
    }
}

impl<'a> Iterator for Iter<'a> {
    type Item = View<'a>;
    fn next(&mut self) -> Option<Self::Item> {
        let mut entry = self.raw()?;
        let mut first = entry.first;
        if !entry.accounting() {
            while let Some(next) = self.raw() {
                if next.position() != entry.position() {
                    self.saved = Some(next);
                    break;
                }
                first = first.min(next.first);
                entry = next;
            }
        }
        Some(View { entry, first })
    }
}

#[cfg(test)]
pub(crate) use tests::allocation::after as fail_pending_allocation_after;

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{journal, value, Version};
    use alloc::vec;
    use std::collections::BTreeMap;

    pub(super) mod allocation {
        use core::cell::Cell;
        use std::alloc::{GlobalAlloc, Layout, System};

        std::thread_local! {
            static FAIL: Cell<Option<usize>> = const { Cell::new(None) };
            static PENDING: Cell<bool> = const { Cell::new(false) };
        }

        pub struct Scope;
        impl Scope {
            pub fn enter() -> Self { PENDING.set(true); Self }
        }
        impl Drop for Scope { fn drop(&mut self) { PENDING.set(false); } }

        struct Allocator;
        #[global_allocator]
        static ALLOCATOR: Allocator = Allocator;

        fn fail() -> bool {
            if !PENDING.try_with(Cell::get).unwrap_or(false) { return false; }
            FAIL.try_with(|f| match f.get() {
                None => false,
                Some(0) => { f.set(None); true }
                Some(n) => { f.set(Some(n - 1)); false }
            }).unwrap_or(false)
        }

        // Successful operations and every deallocation use System's
        // matching layout and pointer; failure returns its null sentinel.
        unsafe impl GlobalAlloc for Allocator {
            unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
                if fail() { core::ptr::null_mut() } else { unsafe { System.alloc(layout) } }
            }
            unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
                if fail() { core::ptr::null_mut() } else { unsafe { System.alloc_zeroed(layout) } }
            }
            unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
                if fail() { core::ptr::null_mut() } else { unsafe { System.realloc(ptr, layout, size) } }
            }
            unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) { unsafe { System.dealloc(ptr, layout) } }
        }

        pub fn after<T>(successful: usize, f: impl FnOnce() -> T) -> T {
            struct Reset;
            impl Drop for Reset { fn drop(&mut self) { FAIL.set(None); } }
            FAIL.set(Some(successful));
            let _reset = Reset;
            f()
        }
    }

    fn update(pos: u64, value: u64) -> Update {
        (Btree::Inodes, 0, Key::new(super::super::kind::INODE_V3, Bpos::new(0, pos, 0)), value.to_le_bytes().to_vec())
    }

    fn add(pending: &mut Pending, seq: u64, updates: Vec<Update>) {
        pending.reserve(updates.len()).unwrap();
        pending.apply(seq, updates);
    }

    fn at(pos: u64) -> Bpos { Bpos::new(0, pos, 0) }


    /// 2047 batches of eight positions each, in eleven runs.
    fn crowded() -> Pending {
        let mut pending = Pending::default();
        for batch in 0..2047u64 { add(&mut pending, batch + 1, (0..8).map(|i| update(batch * 8 + i, i)).collect()); }
        assert_eq!(runs(&pending), 11);
        pending
    }

    fn runs(pending: &Pending) -> usize { pending.runs.iter().flatten().count() }

    #[test]
    fn a_lookup_visits_what_its_position_holds_past_a_materialized_stretch() {
        let mut pending = crowded();
        let captures: Vec<_> = pending.range(Btree::Inodes, 0, at(1001), at(4000)).map(|v| v.capture()).collect();
        for capture in captures { assert!(pending.transfer(capture, |_| ())); }
        let (found, visited) = visits(|| pending.range(Btree::Inodes, 0, at(1000), at(1000)).count());
        assert_eq!(found, 1);
        assert!(visited <= 4 * runs(&pending), "{visited} entries for one position");
    }

    #[test]
    fn durable_visits_only_what_changed_since_it_last_ran() {
        let mut pending = crowded();
        pending.durable(2047);
        add(&mut pending, 3000, vec![update(5, 1)]);
        let (_, visited) = visits(|| pending.durable(3000));
        assert!(visited <= 2 * runs(&pending), "{visited} entries for one update");
        assert_eq!(pending.oldest_pin(), Some(1));
    }

    #[test]
    fn materialized_entries_leave_no_more_dead_than_live_in_a_run() {
        let mut pending = Pending::default();
        for batch in 0..2048u64 { add(&mut pending, batch + 1, (0..8).map(|i| update(batch * 8 + i, i)).collect()); }
        let captures: Vec<_> = pending.range(Btree::Inodes, 0, at(0), at(12_287)).map(|v| v.capture()).collect();
        for capture in captures { assert!(pending.transfer(capture, |_| ())); }
        let live = pending.iter().count();
        assert_eq!(live, 4096);
        let held: usize = pending.runs.iter().flatten().map(|r| r.entries.len()).sum();
        assert!(held <= 2 * live, "{held} entries held for {live} live");
    }

    #[test]
    fn batches_keep_latest_values_and_original_pins_without_cloning_values() {
        let mut pending = Pending::default();
        let mut wanted = BTreeMap::new();
        for batch in 0..257u64 {
            let updates: Vec<_> = (0..7).map(|i| update((batch * 7 + i) % 61, batch * 7 + i)).collect();
            for (_, _, key, value) in &updates {
                let old = wanted.get(&key.pos).map_or(batch + 10, |(_, first, _)| *first);
                wanted.insert(key.pos, (value.as_ptr(), old, value.clone()));
            }
            add(&mut pending, batch + 10, updates);
            let got: Vec<_> = pending.iter().collect();
            assert_eq!(got.len(), wanted.len());
            for (entry, (pos, (ptr, first, bytes))) in got.iter().zip(&wanted) {
                assert_eq!((entry.key().pos, entry.value(), entry.capture().first), (*pos, &bytes[..], *first));
                assert_eq!(entry.value().as_ptr(), *ptr, "owned values move through run merges");
            }
        }
        assert_eq!(pending.oldest_pin(), Some(10));
    }

    #[test]
    fn a_captured_transfer_leaves_a_concurrent_newer_value_pending() {
        let mut pending = Pending::default();
        add(&mut pending, 3, vec![update(1, 11)]);
        add(&mut pending, 4, vec![update(2, 22)]);
        let capture = pending.range(Btree::Inodes, 0, Bpos::new(0, 1, 0), Bpos::new(0, 1, 0)).next().unwrap().capture();
        add(&mut pending, 7, vec![update(1, 33)]);
        let mut destination = None;
        assert!(pending.transfer(capture, |entry| destination = Some((entry.first, entry.value))));
        assert_eq!(destination, Some((3, 11u64.to_le_bytes().to_vec())));
        let latest = pending.range(Btree::Inodes, 0, Bpos::new(0, 1, 0), Bpos::new(0, 1, 0)).next().unwrap();
        assert_eq!((latest.value(), latest.capture().first), (&33u64.to_le_bytes()[..], 7));
        assert_eq!(destination.unwrap().0.min(pending.oldest_pin().unwrap()), 3, "destination owns the transferred dependency");
    }

    #[test]
    fn coalescing_a_captured_value_requires_a_new_capture() {
        let mut pending = Pending::default();
        add(&mut pending, 3, vec![update(1, 11)]);
        let capture = pending.iter().next().unwrap().capture();
        add(&mut pending, 7, vec![update(1, 33)]);
        assert!(!pending.transfer(capture, |_| panic!("superseded capture installed")));
        let latest = pending.iter().next().unwrap();
        assert_eq!((latest.value(), latest.capture().first), (&33u64.to_le_bytes()[..], 3));
    }

    #[test]
    fn durable_replacements_release_old_pins_across_runs_and_keep_captures() {
        let mut pending = Pending::default();
        add(&mut pending, 3, vec![update(1, 11)]);
        add(&mut pending, 4, vec![update(2, 22)]);
        add(&mut pending, 7, vec![update(1, 33)]);
        pending.durable(6);
        assert_eq!(pending.oldest_pin(), Some(3));
        pending.durable(7);
        assert_eq!(pending.oldest_pin(), Some(4));
        assert_eq!(pending.iter().count(), 2);

        add(&mut pending, 8, vec![update(2, 44)]);
        let capture = pending.range(Btree::Inodes, 0, Bpos::new(0, 2, 0), Bpos::new(0, 2, 0)).next().unwrap().capture();
        assert_eq!(capture.first, 4);
        pending.durable(8);
        assert_eq!(pending.oldest_pin(), Some(7), "promoted indices must be re-sorted");
        assert!(pending.transfer(capture, |entry| {
            assert_eq!((entry.first, entry.last, entry.value), (4, 8, 44u64.to_le_bytes().to_vec()));
        }));
        assert_eq!(pending.oldest_pin(), Some(7));
    }

    #[test]
    fn a_forgotten_intermediate_replacement_does_not_release_a_pin() {
        let mut pending = Pending::default();
        add(&mut pending, 3, vec![update(1, 11)]);
        add(&mut pending, 4, vec![update(2, 22)]);
        add(&mut pending, 5, vec![update(1, 33)]);
        add(&mut pending, 6, vec![update(1, 44)]);
        pending.durable(5);
        assert_eq!(pending.oldest_pin(), Some(3));
        pending.durable(6);
        assert_eq!(pending.oldest_pin(), Some(4));
    }

    #[test]
    fn failed_capacity_preflight_preserves_the_visible_batch_and_pin() {
        let mut pending = Pending::default();
        add(&mut pending, 3, vec![update(1, 11)]);
        assert_eq!(pending.reserve(usize::MAX), Err(Error::NoMemory));
        assert_eq!(pending.oldest_pin(), Some(3));
        assert_eq!(pending.iter().next().unwrap().value(), &11u64.to_le_bytes());
        add(&mut pending, 7, vec![update(1, 33)]);
        assert_eq!(pending.iter().next().unwrap().value(), &33u64.to_le_bytes());
    }

    #[test]
    fn any_workspace_allocation_can_fail_before_any_key_or_pin_changes() {
        for successful in 0..3 {
            let mut pending = Pending::default();
            add(&mut pending, 3, vec![update(1, 11)]);
            let extra = pending.incoming.capacity() + 1;
            assert_eq!(allocation::after(successful, || pending.reserve(extra)), Err(Error::NoMemory));
            assert_eq!(pending.oldest_pin(), Some(3));
            let before: Vec<_> = pending.iter().collect();
            assert_eq!(before.len(), 1);
            assert_eq!(before[0].value(), &11u64.to_le_bytes());
            add(&mut pending, 7, vec![update(1, 33)]);
            assert_eq!(pending.iter().next().unwrap().value(), &33u64.to_le_bytes());
        }
    }

    #[test]
    fn accounting_versions_survive_run_merges_and_overlay_only_once() {
        let pos = Bpos::new(0, 9, 0);
        let mut replay = journal::Replay::default();
        for (version, delta) in [(2, 2), (3, 3), (4, 13u64.wrapping_neg()), (5, 7)] {
            let key = Key { version: Version { hi: 0, lo: version }, ..Key::new(super::super::kind::ACCOUNTING, pos) };
            add(&mut replay.pending, version, vec![(Btree::Accounting, 0, key, value::counters(&[delta]))]);
        }
        assert_eq!(replay.pending.iter().count(), 4);
        replay.pending.durable(99);
        assert_eq!(replay.pending.iter().count(), 4);
        assert_eq!(replay.pending.oldest_pin(), Some(2));
        let stored = Key { version: Version { hi: 0, lo: 2 }, ..Key::new(super::super::kind::ACCOUNTING, pos) };
        let once = replay.over(Btree::Accounting, 0, Bpos::MIN, Bpos::MAX, vec![(stored, value::counters(&[10]))]).unwrap();
        assert_eq!(journal::counters(&once[0].1).unwrap(), [7]);
        assert_eq!(once[0].0.version.lo, 5);
        let twice = replay.over(Btree::Accounting, 0, Bpos::MIN, Bpos::MAX, once.clone()).unwrap();
        assert_eq!(once, twice);
    }

    #[test]
    fn deletions_overlay_stored_keys_and_ranges_exclude_neighboring_positions() {
        let mut replay = journal::Replay::default();
        add(&mut replay.pending, 1, vec![update(1, 11), update(2, 22), update(3, 33)]);
        let erased = Key::new(super::super::kind::DELETED, Bpos::new(0, 2, 0));
        add(&mut replay.pending, 2, vec![(Btree::Inodes, 0, erased, Vec::new())]);
        let (_, _, key, value) = update(2, 99);
        assert_eq!(replay.over(Btree::Inodes, 0, key.pos, key.pos, vec![(key, value)]).unwrap(), []);
        let full = replay.over(Btree::Inodes, 0, Bpos::MIN, Bpos::MAX, Vec::new()).unwrap();
        assert_eq!(full.iter().map(|(k, _)| k.pos.offset).collect::<Vec<_>>(), [1, 3]);
    }
}
