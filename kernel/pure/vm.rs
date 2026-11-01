//! A user address space's inventory: every range it holds, what backs it, its
//! protection and whether it is committed. The only record of these; the page
//! tables hold populated frames and nothing else.
//!
//! Ranges are whole 4 KiB pages inside `[floor, top)`, never overlapping.
//! What is not an entry is free, and placement looks at nothing else: the
//! free ranges are kept by address and by length, so finding room, cutting a
//! range out and inserting one are logarithmic in the entries.
//!
//! **Every change is all-or-nothing.** A caller asks what a change would do
//! before making it ([`Inventory::covered`], [`Inventory::is_free`],
//! [`Inventory::place`]), and the change itself cannot be refused.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;

/// The page every range is made of.
pub const PAGE: u64 = 4096;

/// The pages a touch beside a populated page populates at once: an aligned
/// run of 64 KiB.
pub const AROUND: u64 = 16 * PAGE;

/// The pages `[from, to)` a touch of the empty `page` of the range `[start,
/// end)` populates: its own, and where a page beside it in the range is
/// `populated`, every page of its aligned run of [`AROUND`] in the range. So a
/// range touched in order, upward or downward, takes a fault per run and a
/// lone touch takes one page.
pub fn around(start: u64, end: u64, page: u64, populated: impl Fn(u64) -> bool) -> (u64, u64) {
    let beside = |at: u64| (start..end).contains(&at) && populated(at);
    if !beside(page.wrapping_sub(PAGE)) && !beside(page + PAGE) {
        return (page, page + PAGE);
    }
    let run = page & !(AROUND - 1);
    (run.max(start), (run + AROUND).min(end))
}

/// What a page may be used for. No variant is both writable and executable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Prot {
    None,
    Read,
    ReadWrite,
    ReadExec,
}

/// What an access asks of a page.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Access {
    Read,
    Write,
    Exec,
}

impl Prot {
    pub fn allows(self, access: Access) -> bool {
        matches!(
            (self, access),
            (Prot::Read | Prot::ReadWrite | Prot::ReadExec, Access::Read)
                | (Prot::ReadWrite, Access::Write)
                | (Prot::ReadExec, Access::Exec)
        )
    }

    /// Whether every access `self` allows, `max` allows too.
    pub fn within(self, max: Prot) -> bool {
        match self {
            Prot::None => true,
            Prot::Read => max != Prot::None,
            Prot::ReadWrite | Prot::ReadExec => max == self,
        }
    }
}

/// An object a range maps, as the kernel holds it; here only so that a range
/// split in two maps the same object twice and two neighbours mapping one
/// object in order join.
pub trait Object: Clone {
    fn same(&self, other: &Self) -> bool;
}

impl<T: ?Sized> Object for alloc::sync::Arc<T> {
    fn same(&self, other: &Self) -> bool {
        alloc::sync::Arc::ptr_eq(self, other)
    }
}

/// What backs a range.
#[derive(Clone, Debug)]
pub enum Kind<O, S = O> {
    /// Memory of this space's own, its frames recorded in the leaves that map
    /// them. Uncommitted, it is a reservation: no page of it can be touched.
    Private { committed: bool },
    /// `object`'s bytes from `offset` on, never given more than `max` allows.
    Object { object: O, offset: u64, max: Prot },
    /// Memory of this space's own, committed, whose every page starts as a
    /// copy of `source` from `offset` on as far as the range's first `len`
    /// bytes go, and zeros past them: taken at the page's first touch, as a
    /// program's segments are.
    Copy { source: S, offset: u64, len: u64 },
}

#[derive(Clone, Debug)]
pub struct Entry<O, S = O> {
    pub end: u64,
    pub prot: Prot,
    pub kind: Kind<O, S>,
}

impl<O: Object, S: Object> Entry<O, S> {
    /// Whether its pages are this space's own, committed: each owns a frame
    /// once touched, and a promise of one before.
    pub fn committed(&self) -> bool {
        matches!(self.kind, Kind::Private { committed: true } | Kind::Copy { .. })
    }

    /// Whether the range needs page tables: memory a fault or a copy can
    /// populate or reach.
    pub fn backed(&self) -> bool {
        match self.kind {
            Kind::Private { committed } => committed,
            Kind::Object { .. } | Kind::Copy { .. } => true,
        }
    }

    /// The entry from `at` on, which lies inside it.
    fn tail(&self, start: u64, at: u64) -> Self {
        let kind = match &self.kind {
            Kind::Private { committed } => Kind::Private { committed: *committed },
            Kind::Object { object, offset, max } => {
                Kind::Object { object: object.clone(), offset: offset + (at - start), max: *max }
            }
            Kind::Copy { source, offset, len } => {
                Kind::Copy { source: source.clone(), offset: offset + (at - start), len: len.saturating_sub(at - start) }
            }
        };
        Self { end: self.end, prot: self.prot, kind }
    }

    /// Whether `next`, starting where `self` ends, is the same range continued.
    fn continued_by(&self, start: u64, next: &Self) -> bool {
        self.prot == next.prot
            && match (&self.kind, &next.kind) {
                (Kind::Private { committed: a }, Kind::Private { committed: b }) => a == b,
                (Kind::Object { object: a, offset: x, max: m }, Kind::Object { object: b, offset: y, max: n }) => {
                    a.same(b) && x + (self.end - start) == *y && m == n
                }
                (Kind::Copy { source: a, offset: x, len: l }, Kind::Copy { source: b, offset: y, len: k }) => {
                    a.same(b) && x + (self.end - start) == *y && l.saturating_sub(self.end - start) == *k
                }
                _ => false,
            }
    }
}

pub struct Inventory<O, S = O> {
    floor: u64,
    top: u64,
    entries: BTreeMap<u64, Entry<O, S>>,
    /// Free ranges by start, each to its end.
    free: BTreeMap<u64, u64>,
    /// The same ranges by `(length, start)`.
    by_len: BTreeSet<(u64, u64)>,
}

impl<O: Object, S: Object> Inventory<O, S> {
    /// Nothing held in `[floor, top)`, both on pages.
    pub fn new(floor: u64, top: u64) -> Self {
        assert!(floor.is_multiple_of(PAGE) && top.is_multiple_of(PAGE) && floor < top, "vm: {floor:#x}..{top:#x}");
        let mut inventory = Self { floor, top, entries: BTreeMap::new(), free: BTreeMap::new(), by_len: BTreeSet::new() };
        inventory.add_free(floor, top);
        inventory
    }

    pub fn floor(&self) -> u64 {
        self.floor
    }

    pub fn top(&self) -> u64 {
        self.top
    }

    /// Ranges held.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether `[start, end)` is pages inside `[floor, top)`.
    pub fn holds(&self, start: u64, end: u64) -> bool {
        start.is_multiple_of(PAGE) && end.is_multiple_of(PAGE) && self.floor <= start && start < end && end <= self.top
    }

    /// The entry holding `at`, and where it starts.
    pub fn find(&self, at: u64) -> Option<(u64, &Entry<O, S>)> {
        let (&start, entry) = self.entries.range(..=at).next_back()?;
        (at < entry.end).then_some((start, entry))
    }

    /// Every entry `[start, end)` meets, in address order, each with where it
    /// starts: the first may start below `start` and the last end past `end`.
    pub fn meeting(&self, start: u64, end: u64) -> impl Iterator<Item = (u64, &Entry<O, S>)> {
        let first = self.find(start).map(|(s, _)| s).unwrap_or(start);
        self.entries.range(first..end).map(|(&s, e)| (s, e))
    }

    /// Whether no entry meets `[start, end)`.
    pub fn is_free(&self, start: u64, end: u64) -> bool {
        self.meeting(start, end).next().is_none()
    }

    /// Whether entries cover every page of `[start, end)`.
    pub fn covered(&self, start: u64, end: u64) -> bool {
        self.covered_where(start, end, |_, _| true)
    }

    /// Whether entries cover every page of `[start, end)` and each that meets
    /// it answers `ok`, given where it starts: one descent of the tree,
    /// walking down from `end`.
    pub fn covered_where(&self, start: u64, end: u64, ok: impl Fn(u64, &Entry<O, S>) -> bool) -> bool {
        // `[low, end)` is covered by entries that answered `ok`.
        let mut low = end;
        let mut below = self.entries.range(..end).rev();
        while low > start {
            let Some((&s, entry)) = below.next() else { return false };
            if entry.end < low || !ok(s, entry) {
                return false;
            }
            low = s;
        }
        true
    }

    /// Whether any entry meeting `[start, end)` is backed.
    pub fn backed(&self, start: u64, end: u64) -> bool {
        self.meeting(start, end).any(|(_, entry)| entry.backed())
    }

    /// Where `len` bytes of pages aligned to `align`, a power of two of at
    /// least a page, fit: the smallest free range that holds them for any
    /// placement of that alignment inside it, at its lowest aligned address.
    pub fn place(&self, len: u64, align: u64) -> Option<u64> {
        debug_assert!(len.is_multiple_of(PAGE) && len > 0 && align.is_power_of_two() && align >= PAGE);
        let need = len.checked_add(align - PAGE)?;
        let &(_, start) = self.by_len.range((need, 0)..).next()?;
        Some(start.next_multiple_of(align))
    }

    /// Hold `[start, entry.end)`, which is free.
    pub fn insert(&mut self, start: u64, entry: Entry<O, S>) {
        assert!(self.holds(start, entry.end), "vm: {start:#x}..{:#x} is not pages of this space", entry.end);
        let (free_start, free_end) = self.free.range(..=start).next_back().map(|(&s, &e)| (s, e)).expect("vm: inserted over held memory");
        assert!(entry.end <= free_end, "vm: {start:#x}..{:#x} inserted over held memory", entry.end);
        self.take_free(free_start, free_end);
        if free_start < start {
            self.add_free(free_start, start);
        }
        if entry.end < free_end {
            self.add_free(entry.end, free_end);
        }
        let end = entry.end;
        self.entries.insert(start, entry);
        self.join(start, end);
    }

    /// Take every entry out of `[start, end)`, cutting any that crosses its
    /// ends, and answer what was there, in address order.
    pub fn remove(&mut self, start: u64, end: u64) -> Vec<(u64, Entry<O, S>)> {
        self.split_at(start);
        self.split_at(end);
        let starts: Vec<u64> = self.entries.range(start..end).map(|(&s, _)| s).collect();
        let removed: Vec<(u64, Entry<O, S>)> =
            starts.into_iter().map(|s| (s, self.entries.remove(&s).expect("vm: a start just listed"))).collect();
        for (s, entry) in &removed {
            self.release(*s, entry.end);
        }
        removed
    }

    /// Change every entry inside `[start, end)`, cutting any that crosses its
    /// ends, with `change`; neighbours that become one range are joined.
    pub fn update(&mut self, start: u64, end: u64, mut change: impl FnMut(u64, &mut Entry<O, S>)) {
        self.split_at(start);
        self.split_at(end);
        for (&s, entry) in self.entries.range_mut(start..end) {
            change(s, entry);
        }
        self.join(start, end);
    }

    /// Every entry, in address order.
    pub fn entries(&self) -> impl Iterator<Item = (u64, &Entry<O, S>)> {
        self.entries.iter().map(|(&s, e)| (s, e))
    }

    /// Cut the entry holding `at`, if one does and `at` is not its start.
    fn split_at(&mut self, at: u64) {
        let Some((start, entry)) = self.find(at) else { return };
        if start == at {
            return;
        }
        let tail = entry.tail(start, at);
        self.entries.get_mut(&start).expect("vm: the entry just found").end = at;
        self.entries.insert(at, tail);
    }

    /// Join entries meeting `[start, end)` with their neighbours wherever one
    /// continues the other.
    fn join(&mut self, start: u64, end: u64) {
        let first = self.entries.range(..start).next_back().map(|(&s, _)| s).unwrap_or(start);
        let mut at = first;
        loop {
            let Some((&s, entry)) = self.entries.range(at..).next() else { return };
            if s > end {
                return;
            }
            let next = entry.end;
            match self.entries.get(&next) {
                Some(following) if entry.continued_by(s, following) => {
                    let joined_end = following.end;
                    self.entries.remove(&next);
                    self.entries.get_mut(&s).expect("vm: the entry being joined").end = joined_end;
                    at = s;
                }
                _ => at = next,
            }
        }
    }

    /// `[start, end)` was held and is free now.
    fn release(&mut self, start: u64, end: u64) {
        let (mut lo, mut hi) = (start, end);
        if let Some((&s, &e)) = self.free.range(..start).next_back() {
            if e == start {
                self.take_free(s, e);
                lo = s;
            }
        }
        if let Some(&e) = self.free.get(&end) {
            self.take_free(end, e);
            hi = e;
        }
        self.add_free(lo, hi);
    }

    fn add_free(&mut self, start: u64, end: u64) {
        self.free.insert(start, end);
        self.by_len.insert((end - start, start));
    }

    fn take_free(&mut self, start: u64, end: u64) {
        self.free.remove(&start);
        self.by_len.remove(&(end - start, start));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec;
    use std::vec::Vec;

    /// An object by number.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    struct Obj(u32);

    impl Object for Obj {
        fn same(&self, other: &Self) -> bool {
            self == other
        }
    }

    const FLOOR: u64 = 16 * PAGE;
    const TOP: u64 = 1 << 30;

    fn private(end: u64, prot: Prot) -> Entry<Obj> {
        Entry { end, prot, kind: Kind::Private { committed: prot != Prot::None } }
    }

    fn object(end: u64, object: u32, offset: u64) -> Entry<Obj> {
        Entry { end, prot: Prot::ReadWrite, kind: Kind::Object { object: Obj(object), offset, max: Prot::ReadWrite } }
    }

    fn starts(v: &Inventory<Obj>) -> Vec<(u64, u64)> {
        v.entries().map(|(s, e)| (s, e.end)).collect()
    }

    #[test]
    fn an_empty_space_places_at_its_floor_with_the_alignment_asked() {
        let v = Inventory::<Obj>::new(FLOOR, TOP);
        assert_eq!(v.place(PAGE, PAGE), Some(FLOOR));
        assert_eq!(v.place(3 * PAGE, 1 << 20), Some(1 << 20));
        assert_eq!(v.place(TOP - FLOOR, PAGE), Some(FLOOR));
        assert_eq!(v.place(TOP - FLOOR + PAGE, PAGE), None);
        assert_eq!(v.place(TOP - FLOOR, 2 * PAGE), None, "aligned, it no longer fits");
    }

    #[test]
    fn placement_takes_the_smallest_free_range_that_fits() {
        let mut v = Inventory::new(FLOOR, TOP);
        // Free: [FLOOR, a) small, [b, c) of two pages, [d, TOP) large.
        let a = FLOOR + 5 * PAGE;
        v.insert(a, private(a + PAGE, Prot::ReadWrite));
        let (b, c) = (a + PAGE, a + 3 * PAGE);
        v.insert(c, private(c + PAGE, Prot::ReadWrite));
        assert_eq!(v.place(2 * PAGE, PAGE), Some(b));
        assert_eq!(v.place(5 * PAGE, PAGE), Some(FLOOR));
        assert_eq!(v.place(6 * PAGE, PAGE), Some(c + PAGE));
    }

    #[test]
    fn a_removal_cuts_crossing_entries_and_frees_exactly_its_range() {
        let mut v = Inventory::new(FLOOR, TOP);
        v.insert(FLOOR, private(FLOOR + 10 * PAGE, Prot::ReadWrite));
        let removed = v.remove(FLOOR + 3 * PAGE, FLOOR + 5 * PAGE);
        assert_eq!(removed.iter().map(|(s, e)| (*s, e.end)).collect::<Vec<_>>(), vec![(FLOOR + 3 * PAGE, FLOOR + 5 * PAGE)]);
        assert_eq!(starts(&v), vec![(FLOOR, FLOOR + 3 * PAGE), (FLOOR + 5 * PAGE, FLOOR + 10 * PAGE)]);
        assert!(v.is_free(FLOOR + 3 * PAGE, FLOOR + 5 * PAGE));
        assert!(!v.covered(FLOOR, FLOOR + 10 * PAGE));
        assert_eq!(v.place(2 * PAGE, PAGE), Some(FLOOR + 3 * PAGE), "the hole is the best fit");
    }

    #[test]
    fn a_removal_over_entries_and_holes_takes_every_entry_it_meets() {
        let mut v = Inventory::new(FLOOR, TOP);
        v.insert(FLOOR, private(FLOOR + 2 * PAGE, Prot::Read));
        v.insert(FLOOR + 4 * PAGE, object(FLOOR + 8 * PAGE, 1, 0));
        let removed = v.remove(FLOOR + PAGE, FLOOR + 6 * PAGE);
        assert_eq!(removed.len(), 2);
        match &removed[1].1.kind {
            Kind::Object { offset, .. } => assert_eq!(*offset, 0),
            other => panic!("{other:?}"),
        }
        match &v.find(FLOOR + 6 * PAGE).unwrap().1.kind {
            Kind::Object { offset, .. } => assert_eq!(*offset, 2 * PAGE, "the object's tail keeps its offset"),
            other => panic!("{other:?}"),
        }
        assert_eq!(starts(&v), vec![(FLOOR, FLOOR + PAGE), (FLOOR + 6 * PAGE, FLOOR + 8 * PAGE)]);
    }

    /// A copied range cut in two keeps, in its tail, where the copy goes on
    /// and how much of it is left; a tail past the copied bytes is all zeros;
    /// and the halves made one again are the range they were.
    #[test]
    fn a_copied_range_cut_keeps_its_source_offset_and_length() {
        let mut v = Inventory::new(FLOOR, TOP);
        let copy = |end, offset, len| Entry { end, prot: Prot::ReadWrite, kind: Kind::Copy { source: Obj(7), offset, len } };
        v.insert(FLOOR, copy(FLOOR + 4 * PAGE, 3 * PAGE, PAGE + 100));
        v.update(FLOOR + PAGE, FLOOR + 2 * PAGE, |_, e| e.prot = Prot::Read);
        let kind = |at| v.find(at).unwrap().1.kind.clone();
        assert!(matches!(kind(FLOOR), Kind::Copy { offset, len, .. } if offset == 3 * PAGE && len == PAGE + 100));
        assert!(matches!(kind(FLOOR + PAGE), Kind::Copy { offset, len, .. } if offset == 4 * PAGE && len == 100));
        assert!(matches!(kind(FLOOR + 2 * PAGE), Kind::Copy { offset, len, .. } if offset == 5 * PAGE && len == 0));
        assert!(v.find(FLOOR).unwrap().1.committed(), "a copied range is committed");
        v.update(FLOOR + PAGE, FLOOR + 2 * PAGE, |_, e| e.prot = Prot::ReadWrite);
        assert_eq!(starts(&v), vec![(FLOOR, FLOOR + 4 * PAGE)], "the cut halves did not join again");
    }

    #[test]
    fn neighbours_that_continue_each_other_join() {
        let mut v = Inventory::new(FLOOR, TOP);
        v.insert(FLOOR, private(FLOOR + PAGE, Prot::ReadWrite));
        v.insert(FLOOR + PAGE, private(FLOOR + 2 * PAGE, Prot::ReadWrite));
        assert_eq!(starts(&v), vec![(FLOOR, FLOOR + 2 * PAGE)]);
        v.insert(FLOOR + 2 * PAGE, private(FLOOR + 3 * PAGE, Prot::Read));
        assert_eq!(v.len(), 2, "a different protection is another range");
        v.update(FLOOR + 2 * PAGE, FLOOR + 3 * PAGE, |_, e| e.prot = Prot::ReadWrite);
        assert_eq!(starts(&v), vec![(FLOOR, FLOOR + 3 * PAGE)]);
        // An object joins only where its offsets run on.
        v.insert(FLOOR + 10 * PAGE, object(FLOOR + 11 * PAGE, 7, 0));
        v.insert(FLOOR + 11 * PAGE, object(FLOOR + 12 * PAGE, 7, 2 * PAGE));
        v.insert(FLOOR + 12 * PAGE, object(FLOOR + 13 * PAGE, 7, 3 * PAGE));
        assert_eq!(v.len(), 3);
    }

    #[test]
    fn an_update_changes_only_its_range_and_joins_back() {
        let mut v = Inventory::new(FLOOR, TOP);
        v.insert(FLOOR, private(FLOOR + 8 * PAGE, Prot::ReadWrite));
        v.update(FLOOR + 2 * PAGE, FLOOR + 4 * PAGE, |_, e| e.prot = Prot::Read);
        assert_eq!(v.len(), 3);
        assert_eq!(v.find(FLOOR + 3 * PAGE).unwrap().1.prot, Prot::Read);
        assert_eq!(v.find(FLOOR + 4 * PAGE).unwrap().1.prot, Prot::ReadWrite);
        v.update(FLOOR, FLOOR + 8 * PAGE, |_, e| e.prot = Prot::ReadWrite);
        assert_eq!(starts(&v), vec![(FLOOR, FLOOR + 8 * PAGE)]);
    }

    #[test]
    fn backing_is_a_commitment_or_an_object() {
        let mut v = Inventory::new(FLOOR, TOP);
        v.insert(FLOOR, private(FLOOR + PAGE, Prot::None));
        assert!(!v.backed(FLOOR, FLOOR + PAGE));
        v.insert(FLOOR + PAGE, object(FLOOR + 2 * PAGE, 1, 0));
        assert!(v.backed(FLOOR, FLOOR + 2 * PAGE));
        v.update(FLOOR, FLOOR + PAGE, |_, e| e.kind = Kind::Private { committed: true });
        assert!(v.backed(FLOOR, FLOOR + PAGE));
    }

    #[test]
    #[should_panic(expected = "over held memory")]
    fn an_insert_over_an_entry_is_a_kernel_bug() {
        let mut v = Inventory::new(FLOOR, TOP);
        v.insert(FLOOR, private(FLOOR + 2 * PAGE, Prot::ReadWrite));
        v.insert(FLOOR + PAGE, private(FLOOR + 3 * PAGE, Prot::ReadWrite));
    }

    #[test]
    fn protection_allows_its_accesses_and_bounds_by_its_maximum() {
        use Access::*;
        assert!(Prot::ReadWrite.allows(Write) && Prot::ReadWrite.allows(Read) && !Prot::ReadWrite.allows(Exec));
        assert!(Prot::ReadExec.allows(Exec) && !Prot::ReadExec.allows(Write));
        assert!(!Prot::None.allows(Read));
        assert!(Prot::Read.within(Prot::ReadWrite) && !Prot::ReadWrite.within(Prot::Read));
        assert!(!Prot::ReadExec.within(Prot::ReadWrite) && Prot::None.within(Prot::None));
    }

    /// The inventory against a page map of the same space, through a
    /// deterministic mix of every change: what is held, what is free and what
    /// the free index can place always agree.
    #[test]
    fn a_mixed_load_agrees_with_a_page_map() {
        const PAGES: u64 = 512;
        let top = FLOOR + PAGES * PAGE;
        let mut v = Inventory::<Obj>::new(FLOOR, top);
        // Each page's (prot, committed) or None for free.
        let mut map: Vec<Option<(Prot, bool)>> = vec![None; PAGES as usize];
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let prots = [Prot::None, Prot::Read, Prot::ReadWrite, Prot::ReadExec];
        for round in 0..4000 {
            let a = next() % PAGES;
            let n = 1 + next() % 16;
            let (start, end) = (FLOOR + a * PAGE, FLOOR + (a + n).min(PAGES) * PAGE);
            let page = |at: u64| ((at - FLOOR) / PAGE) as usize;
            match next() % 4 {
                0 => {
                    if let Some(at) = v.place(n * PAGE, PAGE) {
                        let prot = prots[(next() % 4) as usize];
                        v.insert(at, private(at + n * PAGE, prot));
                        for p in &mut map[page(at)..page(at + n * PAGE)] {
                            assert!(p.is_none(), "round {round}: placed over a held page");
                            *p = Some((prot, prot != Prot::None));
                        }
                    }
                }
                1 => {
                    v.remove(start, end);
                    map[page(start)..page(end)].iter_mut().for_each(|p| *p = None);
                }
                2 if v.covered(start, end) => {
                    let prot = prots[(next() % 4) as usize];
                    v.update(start, end, |_, e| {
                        e.prot = prot;
                        e.kind = Kind::Private { committed: prot != Prot::None };
                    });
                    map[page(start)..page(end)].iter_mut().for_each(|p| *p = Some((prot, prot != Prot::None)));
                }
                _ => {
                    if v.is_free(start, end) {
                        let prot = prots[(next() % 4) as usize];
                        v.insert(start, private(end, prot));
                        map[page(start)..page(end)].iter_mut().for_each(|p| *p = Some((prot, prot != Prot::None)));
                    }
                }
            }
            // Coverage of the round's range, and of its writable pages, as the map says.
            let pages = &map[page(start)..page(end)];
            assert_eq!(v.covered(start, end), pages.iter().all(Option::is_some), "round {round}: covered");
            assert_eq!(
                v.covered_where(start, end, |_, e| e.prot == Prot::ReadWrite),
                pages.iter().all(|p| p.is_some_and(|(prot, _)| prot == Prot::ReadWrite)),
                "round {round}: covered by writable entries"
            );
            // Every page as the map says, and no two neighbours that could join.
            for p in 0..PAGES {
                let at = FLOOR + p * PAGE;
                let held = v.find(at).map(|(_, e)| (e.prot, e.backed()));
                assert_eq!(held, map[p as usize], "round {round}: page {p}");
            }
            let entries: Vec<(u64, &Entry<Obj>)> = v.entries().collect();
            for pair in entries.windows(2) {
                let ((s, a), (t, b)) = (pair[0], pair[1]);
                assert!(!(a.end == t && a.continued_by(s, b)), "round {round}: {s:#x} and {t:#x} were left apart");
            }
            // The free index is exactly the gaps.
            let longest_gap = {
                let (mut best, mut run) = (0, 0);
                for p in &map {
                    run = if p.is_none() { run + 1 } else { 0 };
                    best = best.max(run);
                }
                best
            };
            assert!(longest_gap == 0 || v.place(longest_gap * PAGE, PAGE).is_some(), "round {round}");
            assert_eq!(v.place((longest_gap + 1) * PAGE, PAGE), None, "round {round}");
        }
    }

    #[test]
    fn a_lone_touch_populates_its_own_page() {
        let (start, end) = (FLOOR, FLOOR + 64 * AROUND);
        let page = start + 5 * AROUND + 3 * PAGE;
        assert_eq!(around(start, end, page, |_| false), (page, page + PAGE));
        // A page two away is not beside it.
        assert_eq!(around(start, end, page, |at| at == page + 2 * PAGE), (page, page + PAGE));
    }

    #[test]
    fn a_touch_beside_a_populated_page_populates_its_run_upward_or_downward() {
        let (start, end) = (FLOOR, FLOOR + 64 * AROUND);
        let run = start + 5 * AROUND;
        let after_a_run = |at| at < run;
        assert_eq!(around(start, end, run, after_a_run), (run, run + AROUND), "the next run, touched in order");
        let below_a_run = |at| at >= run + AROUND;
        let last = run + AROUND - PAGE;
        assert_eq!(around(start, end, last, below_a_run), (run, run + AROUND), "a stack's next run down");
    }

    #[test]
    fn a_run_stops_at_the_ranges_ends_and_a_neighbour_outside_it_counts_for_nothing() {
        let (start, end) = (FLOOR + 3 * PAGE, FLOOR + AROUND + 2 * PAGE);
        assert_eq!(around(start, end, start + PAGE, |at| at == start), (start, FLOOR + AROUND));
        assert_eq!(around(start, end, end - PAGE, |at| at == end - 2 * PAGE), (FLOOR + AROUND, end));
        // Populated memory just past either end is another range's.
        assert_eq!(around(start, end, start, |at| at < start), (start, start + PAGE));
        assert_eq!(around(start, end, end - PAGE, |at| at >= end), (end - PAGE, end));
    }
}
