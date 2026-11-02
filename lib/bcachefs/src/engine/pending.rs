//! Journal-owned keys, independent of the leaves that will materialize them.

use alloc::vec::Vec;
use alloc::collections::BTreeMap;

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
}

struct Run {
    entries: Vec<Entry>,
    pins: Vec<usize>,
    oldest: usize,
}

impl Run {
    fn oldest(&self) -> Option<&Entry> {
        self.pins.get(self.oldest).map(|&i| &self.entries[i])
    }

    fn retire(&mut self) {
        while self.pins.get(self.oldest).is_some_and(|&i| !self.entries[i].live) { self.oldest += 1; }
    }
}

impl Entry {
    fn position(&self) -> Position { (self.btree, self.level, self.key.pos) }
    fn ordering(&self) -> (Position, u64) { (self.position(), self.order) }
    fn accounting(&self) -> bool { self.btree == Btree::Accounting && self.level == 0 }
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

impl View<'_> {
    pub fn position(&self) -> Position { self.entry.position() }
    pub fn key(&self) -> &Key { &self.entry.key }
    pub fn value(&self) -> &[u8] { &self.entry.value }
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
    pins: Vec<usize>,
    next: u64,
    prepared: usize,
}

impl Default for Pending {
    fn default() -> Self {
        Self { runs: core::array::from_fn(|_| None), incoming: Vec::new(), merged: Vec::new(), pins: Vec::new(), next: 0, prepared: 0 }
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
            total = total.checked_add(run.entries.iter().filter(|e| e.live).count()).ok_or(Error::NoMemory)?;
        }
        if self.runs.iter().all(Option::is_some) || u64::try_from(additional).ok().and_then(|n| self.next.checked_add(n)).is_none() {
            return Err(Error::NoMemory);
        }
        self.incoming.try_reserve(additional).map_err(|_| Error::NoMemory)?;
        self.merged.try_reserve(total).map_err(|_| Error::NoMemory)?;
        self.pins.try_reserve(total).map_err(|_| Error::NoMemory)?;
        self.prepared = additional;
        Ok(())
    }

    /// `reserve` funded this batch. Values move; sorting and merging allocate
    /// nothing. Equal positions retain the latest value and earliest pin.
    pub fn apply(&mut self, seq: u64, updates: impl IntoIterator<Item = Update>) {
        for (btree, level, key, value) in updates {
            assert!(self.incoming.len() < self.prepared, "pending batch exceeded its reservation");
            self.incoming.push(Entry { btree, level, key, value, first: seq, last: seq, order: self.next, live: true });
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
            if !entry.accounting() && self.merged.last().is_some_and(|old| old.position() == entry.position()) {
                entry.first = entry.first.min(self.merged.last().unwrap().first);
                *self.merged.last_mut().unwrap() = entry;
            } else {
                self.merged.push(entry);
            }
        }
        self.incoming.clear();
        for run in &mut self.runs[..level] { *run = None; }
        self.pins.extend(0..self.merged.len());
        self.pins.sort_unstable_by_key(|&i| (self.merged[i].first, self.merged[i].order));
        self.runs[level] = Some(Run { entries: core::mem::take(&mut self.merged), pins: core::mem::take(&mut self.pins), oldest: 0 });
    }

    pub fn oldest_pin(&self) -> Option<u64> {
        self.oldest().map(|c| c.first)
    }

    pub fn oldest(&self) -> Option<Capture> {
        self.runs.iter().flatten().filter_map(Run::oldest).min_by_key(|e| (e.first, e.order))
            .map(|e| Capture { position: e.position(), order: e.order, first: e.first })
    }

    #[cfg(test)]
    pub fn allocated_bytes(&self) -> usize {
        let entries = |entries: &Vec<Entry>| entries.capacity() * core::mem::size_of::<Entry>() + entries.iter().map(|e| e.value.capacity()).sum::<usize>();
        core::mem::size_of::<Self>() + entries(&self.incoming) + entries(&self.merged) + self.pins.capacity() * core::mem::size_of::<usize>()
            + self.runs.iter().flatten().map(|r| entries(&r.entries) + r.pins.capacity() * core::mem::size_of::<usize>()).sum::<usize>()
    }

    #[cfg(test)]
    pub fn iter(&self) -> Iter<'_> { self.between(None) }

    pub fn range(&self, btree: Btree, level: u8, min: Bpos, max: Bpos) -> Iter<'_> {
        self.between(Some(((btree, level, min), (btree, level, max))))
    }

    /// The logical range, preserving zero accounting values and their
    /// versions so materialization cannot apply an old delta twice.
    pub fn over(&self, btree: Btree, level: u8, min: Bpos, max: Bpos, keys: Vec<(Key, Vec<u8>)>) -> Result<Vec<(Key, Vec<u8>)>> {
        let mut merged: BTreeMap<_, _> = keys.into_iter().map(|(k, v)| (k.pos, (k, v))).collect();
        let mut pending = self.range(btree, level, min, max).peekable();
        if btree == Btree::Accounting && level == 0 {
            while let Some(first) = pending.peek() {
                let pos = first.key().pos;
                let (mut key, mut sums) = match merged.remove(&pos) {
                    Some((key, value)) => (key, journal::counters(&value)?),
                    None => (Key::new(kind::ACCOUNTING, pos), Vec::new()),
                };
                let since = key.version;
                while pending.peek().is_some_and(|entry| entry.key().pos == pos) {
                    let entry = pending.next().unwrap();
                    let version = entry.key().version;
                    if version <= since { continue; }
                    let delta = journal::counters(entry.value())?;
                    sums.resize(sums.len().max(delta.len()), 0);
                    for (sum, d) in sums.iter_mut().zip(delta) { *sum = sum.wrapping_add(d); }
                    key.version = key.version.max(version);
                }
                merged.insert(pos, (key, super::value::counters(&sums)));
            }
        } else {
            for entry in pending {
                let key = *entry.key();
                if key.kind == kind::DELETED { merged.remove(&key.pos); }
                else { merged.insert(key.pos, (key, entry.value().to_vec())); }
            }
        }
        Ok(merged.into_values().collect())
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
        let source = self.runs.iter_mut().flatten().find_map(|run| {
            let at = run.entries.partition_point(|e| e.ordering() < wanted);
            run.entries.get_mut(at).filter(|e| e.live && e.ordering() == wanted)
        });
        let Some(source) = source else { return false };
        let accounting = source.accounting();
        let value = core::mem::take(&mut source.value);
        install(Transferred { key: source.key, value, first: capture.first, last: source.last });
        for run in self.runs.iter_mut().flatten() {
            let at = run.entries.partition_point(|e| e.position() < capture.position);
            for entry in run.entries[at..].iter_mut().take_while(|e| e.position() == capture.position && e.order <= capture.order) {
                if entry.live && (entry.order == capture.order || !accounting) {
                    entry.live = false;
                    entry.value = Vec::new();
                }
            }
            run.retire();
        }
        true
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
            while run.get(self.at[i]).is_some_and(|e| !e.live) { self.at[i] += 1; }
            if let Some(entry) = run.get(self.at[i]) {
                if self.end.is_none_or(|end| entry.position() <= end) && best.is_none_or(|old| entry.ordering() < old) {
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
