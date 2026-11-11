//! The table's state: every decision the table makes, which a host tests and
//! `model` checks under loom. Nothing in it panics.
//!
//! A descriptor's word names an object's slot, and a slot counts its
//! references: the descriptors naming it and the calls holding it.
//!
//! - **A slot is free while its count is 0**, and only the [`Writer`] takes a
//!   count off 0: it writes the object, then stores the count. Whoever holds a
//!   reference reads the object, so nothing is written under a reader.
//! - **A call counts itself onto a slot only while the count is not 0**, and
//!   keeps the reference only if the descriptor still names that slot. So the
//!   object a call holds is one its descriptor named during the call, and a
//!   slot is reused only after its last hold.
//! - **Whoever takes a count to 0 is answered the object**, read while it
//!   still held its reference, and lets it go: once, after the last call.
//! - **A descriptor changes in one store**, so a call beside a `dup2` holds
//!   the old object or the new, and a closed number is free at once.

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicU32, Ordering};

use super::{Error, Fd};

#[cfg(test)]
mod model;

/// A descriptor's word: 0 for a free number, else its object's slot plus one,
/// with [`INHERITABLE`] set for an inheritable one. The empty table is all
/// zeros, so it costs a program no initialised data.
const INHERITABLE: u32 = 1 << 31;

/// A shared word of the table: an atomic, or a model's.
pub(super) trait Word {
    fn load(&self, order: Ordering) -> u32;
    fn store(&self, value: u32, order: Ordering);
    fn compare_exchange(&self, current: u32, new: u32, success: Ordering, failure: Ordering) -> Result<u32, u32>;
    fn fetch_sub(&self, value: u32, order: Ordering) -> u32;
}

impl Word for AtomicU32 {
    fn load(&self, order: Ordering) -> u32 {
        AtomicU32::load(self, order)
    }
    fn store(&self, value: u32, order: Ordering) {
        AtomicU32::store(self, value, order)
    }
    fn compare_exchange(&self, current: u32, new: u32, success: Ordering, failure: Ordering) -> Result<u32, u32> {
        AtomicU32::compare_exchange(self, current, new, success, failure)
    }
    fn fetch_sub(&self, value: u32, order: Ordering) -> u32 {
        AtomicU32::fetch_sub(self, value, order)
    }
}

/// Where a slot keeps its object: a cell, or a model's, which checks each
/// access against the others.
pub(super) trait Place {
    type Object: Copy;

    /// # Safety
    /// The place was written, and no write is concurrent.
    unsafe fn read(&self) -> Self::Object;

    /// # Safety
    /// No other access is concurrent.
    unsafe fn write(&self, object: Self::Object);
}

pub(super) struct Cell<O>(UnsafeCell<MaybeUninit<O>>);

// SAFETY: reached only through `Place`, whose callers keep a write alone.
unsafe impl<O: Send + Sync> Sync for Cell<O> {}

impl<O: Copy> Place for Cell<O> {
    type Object = O;

    unsafe fn read(&self) -> O {
        // SAFETY: the caller's: written, and not being written.
        unsafe { (*self.0.get()).assume_init() }
    }

    unsafe fn write(&self, object: O) {
        // SAFETY: the caller's: nothing else reaches the cell.
        unsafe { (*self.0.get()).write(object) };
    }
}

/// Why a table operation failed: refused, or the table found inconsistent,
/// which the owner panics on only once its lock is released.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Fault {
    Refused(Error),
    Broken,
}

impl From<Error> for Fault {
    fn from(e: Error) -> Self {
        Self::Refused(e)
    }
}

/// An object's slot, on a cache line of its own: calls on two descriptors
/// write no line they share.
#[repr(align(64))]
struct Slot<W, P> {
    /// The slot's count of references.
    refs: W,
    object: P,
}

pub(super) struct Table<W, P, const N: usize> {
    fds: [W; N],
    slots: [Slot<W, P>; N],
}

impl<O, const N: usize> Table<AtomicU32, Cell<O>, N> {
    pub(super) const fn new() -> Self {
        assert!(N < INHERITABLE as usize);
        Self {
            fds: [const { AtomicU32::new(0) }; N],
            slots: [const { Slot { refs: AtomicU32::new(0), object: Cell(UnsafeCell::new(MaybeUninit::uninit())) } }; N],
        }
    }
}

impl<W: Word, P: Place, const N: usize> Table<W, P, N> {
    fn word(&self, fd: Fd) -> Result<&W, Error> {
        usize::try_from(fd).ok().and_then(|i| self.fds.get(i)).ok_or(Error::BadFd)
    }

    /// The slot a descriptor's word names.
    fn named(word: u32) -> Result<usize, Fault> {
        if word == 0 {
            return Err(Error::BadFd.into());
        }
        ((word & !INHERITABLE) as usize).checked_sub(1).filter(|&slot| slot < N).ok_or(Fault::Broken)
    }

    /// One reference more on `slot`, unless it is free.
    fn reference(&self, slot: usize) -> Result<bool, Fault> {
        let refs = &self.slots.get(slot).ok_or(Fault::Broken)?.refs;
        let mut count = refs.load(Ordering::Acquire);
        loop {
            if count == 0 && !cfg!(feature = "fd-hold-revives") {
                return Ok(false);
            }
            let more = count.checked_add(1).ok_or(Fault::Broken)?;
            match refs.compare_exchange(count, more, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return Ok(true),
                Err(now) => count = now,
            }
        }
    }

    /// One reference fewer on `slot`: its object, once none is left.
    ///
    /// # Safety
    /// The caller gives up a reference it holds on `slot`.
    unsafe fn unref(&self, slot: usize) -> Result<Option<P::Object>, Fault> {
        let slot = self.slots.get(slot).ok_or(Fault::Broken)?;
        // SAFETY: the caller's reference keeps the slot from the writer, and
        // it is given up only below.
        let object = unsafe { slot.object.read() };
        match slot.refs.fetch_sub(1, Ordering::AcqRel) {
            0 => Err(Fault::Broken),
            1 => Ok(Some(object)),
            _ => Ok(None),
        }
    }

    /// `fd`'s object, held until [`Self::unuse`] of the token answered. An
    /// object whose last reference a lost race left with this call goes to
    /// `let_go`.
    pub(super) fn acquire(&self, fd: Fd, mut let_go: impl FnMut(P::Object)) -> Result<(usize, P::Object), Fault> {
        let word = self.word(fd)?;
        loop {
            let slot = Self::named(word.load(Ordering::Acquire))?;
            // A free slot was closed since the word was read, which the next
            // read of the word shows.
            if !self.reference(slot)? {
                continue;
            }
            // SAFETY: the reference just taken.
            let object = unsafe { self.slots[slot].object.read() };
            if cfg!(feature = "fd-hold-unchecked") || Self::named(word.load(Ordering::Acquire)) == Ok(slot) {
                return Ok((slot, object));
            }
            // SAFETY: the reference just taken.
            if let Some(last) = unsafe { self.unref(slot) }? {
                let_go(last);
            }
        }
    }

    /// A call done with the object `token` names: the object, if nothing
    /// names it any more.
    ///
    /// # Safety
    /// `token` is one this table's [`Self::acquire`] answered, given back
    /// once.
    pub(super) unsafe fn unuse(&self, token: usize) -> Result<Option<P::Object>, Fault> {
        // SAFETY: the call's reference.
        unsafe { self.unref(token) }
    }

    pub(super) fn inheritable(&self, fd: Fd) -> Result<bool, Fault> {
        let word = self.word(fd)?.load(Ordering::Acquire);
        Self::named(word).map(|_| word & INHERITABLE != 0)
    }

    /// The table's one writer.
    ///
    /// # Safety
    /// No other [`Writer`] of this table exists while this one does.
    pub(super) unsafe fn writer(&self) -> Writer<'_, W, P, N> {
        Writer(self)
    }
}

/// The right to change what the table's descriptors name. A removal answers
/// the object it released, for the caller to let go outside its lock.
pub(super) struct Writer<'a, W, P, const N: usize>(&'a Table<W, P, N>);

impl<W: Word, P: Place, const N: usize> Writer<'_, W, P, N> {
    /// `fd`'s index, slot and flag.
    fn entry(&self, fd: Fd) -> Result<(usize, usize, bool), Fault> {
        let i = usize::try_from(fd).map_err(|_| Error::BadFd)?;
        let word = self.0.fds.get(i).ok_or(Error::BadFd)?.load(Ordering::Relaxed);
        let slot = Table::<W, P, N>::named(word)?;
        // Only this writer takes a name's reference away.
        if self.0.slots[slot].refs.load(Ordering::Relaxed) == 0 {
            return Err(Fault::Broken);
        }
        Ok((i, slot, word & INHERITABLE != 0))
    }

    fn set(&mut self, i: usize, slot: usize, inheritable: bool) -> Result<(), Fault> {
        let index = u32::try_from(slot).ok().filter(|&s| s < INHERITABLE - 1).ok_or(Fault::Broken)?;
        let flag = if inheritable { INHERITABLE } else { 0 };
        self.0.fds.get(i).ok_or(Fault::Broken)?.store((index + 1) | flag, Ordering::Release);
        Ok(())
    }

    fn is_free(&self, i: usize) -> bool {
        self.0.fds.get(i).is_some_and(|word| word.load(Ordering::Relaxed) == 0)
    }

    fn lowest_free(&self, floor: Fd) -> Result<usize, Error> {
        let floor = usize::try_from(floor).map_err(|_| Error::Full)?;
        (floor..N).find(|&i| self.is_free(i)).ok_or(Error::Full)
    }

    /// A free slot holding `object`, referenced once.
    fn hold(&mut self, object: P::Object) -> Result<usize, Error> {
        // The load that finds a slot free is ordered after its last reader.
        let index = self.0.slots.iter().position(|slot| slot.refs.load(Ordering::Acquire) == 0).ok_or(Error::Full)?;
        let slot = &self.0.slots[index];
        // SAFETY: a free slot is this writer's alone until its count is stored.
        unsafe { slot.object.write(object) };
        let publish = if cfg!(feature = "fd-publish-relaxed") { Ordering::Relaxed } else { Ordering::Release };
        slot.refs.store(1, publish);
        Ok(index)
    }

    /// One reference more on a slot a descriptor names.
    fn reference(&mut self, slot: usize) -> Result<(), Fault> {
        self.0.reference(slot)?.then_some(()).ok_or(Fault::Broken)
    }

    /// One reference fewer on a slot [`Self::entry`] answered, whose name
    /// was just taken away.
    fn unname(&mut self, slot: usize) -> Result<Option<P::Object>, Fault> {
        // SAFETY: the name's reference, which `entry` found counted.
        unsafe { self.0.unref(slot) }
    }

    /// Name `object` by the lowest free descriptor at or above `floor`.
    pub(super) fn install(&mut self, object: P::Object, inheritable: bool, floor: Fd) -> Result<Fd, Fault> {
        let fd = self.lowest_free(floor)?;
        let slot = self.hold(object)?;
        self.set(fd, slot, inheritable)?;
        Ok(fd as Fd)
    }

    /// Name `object` by `fd` exactly: `BadFd` for a number past the table
    /// or already open.
    pub(super) fn install_at(&mut self, fd: Fd, object: P::Object, inheritable: bool) -> Result<(), Fault> {
        let i = usize::try_from(fd).ok().filter(|&i| self.is_free(i)).ok_or(Error::BadFd)?;
        let slot = self.hold(object)?;
        self.set(i, slot, inheritable)
    }

    /// An endowed alias at its exact number, without displacing an earlier
    /// endowment. Both descriptors share one object and one final release.
    pub(super) fn alias_at(&mut self, source: Fd, fd: Fd) -> Result<(), Fault> {
        let i = usize::try_from(fd).ok().filter(|&i| self.is_free(i)).ok_or(Error::BadFd)?;
        let (_, slot, _) = self.entry(source)?;
        self.reference(slot)?;
        self.set(i, slot, true)
    }

    /// A second descriptor on `fd`'s object, the lowest free at or above
    /// `floor`.
    pub(super) fn dup(&mut self, fd: Fd, floor: Fd, inheritable: bool) -> Result<Fd, Fault> {
        let (_, slot, _) = self.entry(fd)?;
        let new = self.lowest_free(floor)?;
        self.reference(slot)?;
        self.set(new, slot, inheritable)?;
        Ok(new as Fd)
    }

    /// `new` on `old`'s object, whatever `new` named before: that object,
    /// if this was its last reference. `new == old` changes nothing.
    pub(super) fn dup2(&mut self, old: Fd, new: Fd, inheritable: bool) -> Result<Option<P::Object>, Fault> {
        let (_, slot, _) = self.entry(old)?;
        let i = usize::try_from(new).ok().filter(|&i| i < N).ok_or(Error::BadFd)?;
        if new == old {
            return Ok(None);
        }
        let displaced = self.entry(new).ok();
        self.reference(slot)?;
        self.set(i, slot, inheritable)?;
        match displaced {
            Some((_, displaced, _)) => self.unname(displaced),
            None => Ok(None),
        }
    }

    /// Forget `fd`: its object, if this was its last reference.
    pub(super) fn close(&mut self, fd: Fd) -> Result<Option<P::Object>, Fault> {
        let (i, slot, _) = self.entry(fd)?;
        self.0.fds.get(i).ok_or(Fault::Broken)?.store(0, Ordering::Release);
        self.unname(slot)
    }

    /// Make `fd` name `object` from here on, in place: the object it named,
    /// if this was its last reference. Its flag stays.
    pub(super) fn replace(&mut self, fd: Fd, object: P::Object) -> Result<Option<P::Object>, Fault> {
        let (i, old, inheritable) = self.entry(fd)?;
        let slot = self.hold(object)?;
        self.set(i, slot, inheritable)?;
        self.unname(old)
    }

    pub(super) fn set_inheritable(&mut self, fd: Fd, inheritable: bool) -> Result<(), Fault> {
        let (i, slot, _) = self.entry(fd)?;
        self.set(i, slot, inheritable)
    }
}

#[cfg(test)]
mod tests {
    use super::super::Record;
    use super::*;

    extern "C" fn nothing(_: u64) {}

    fn o(word: u64) -> Record {
        Record { handle: toyos_abi::HANDLE_INVALID, tag: super::super::UNTAGGED, word, release: Some(nothing) }
    }

    type T = Table<AtomicU32, Cell<Record>, 8>;
    type W<'a> = Writer<'a, AtomicU32, Cell<Record>, 8>;

    /// These tests' one writer of `t`.
    fn writer(t: &T) -> W<'_> {
        // SAFETY: each test makes one.
        unsafe { t.writer() }
    }

    /// The word of what a removal released, which names it in these tests.
    fn w(released: Result<Option<Record>, Fault>) -> Result<Option<u64>, Fault> {
        released.map(|o| o.map(|o| o.word))
    }

    fn acquire(t: &T, fd: Fd) -> Result<(usize, Record), Fault> {
        t.acquire(fd, |_| panic!("a call with no race was left an object"))
    }

    /// The objects something still references.
    fn held(t: &T) -> usize {
        t.slots.iter().filter(|slot| slot.refs.load(Ordering::Relaxed) != 0).count()
    }

    #[test]
    fn descriptors_are_numbered_lowest_free_from_the_floor() {
        let t = T::new();
        let mut t = writer(&t);
        assert_eq!(t.install(o(1), false, 0), Ok(0));
        assert_eq!(t.install(o(2), false, 0), Ok(1));
        assert_eq!(t.install(o(3), false, 5), Ok(5));
        assert_eq!(w(t.close(0)), Ok(Some(1)));
        assert_eq!(t.install(o(4), false, 0), Ok(0), "a closed number is the lowest free again");
        assert_eq!(t.install(o(5), false, 8), Err(Error::Full.into()));
        assert_eq!(t.install(o(5), false, -1), Err(Error::Full.into()));
    }

    /// Descriptors on one object release it with the last of them, and each
    /// keeps a flag of its own.
    #[test]
    fn an_object_goes_with_its_last_descriptor_and_flags_are_per_descriptor() {
        let table = T::new();
        let mut t = writer(&table);
        let a = t.install(o(7), true, 0).unwrap();
        let b = t.dup(a, 0, false).unwrap();
        assert_eq!((table.inheritable(a), table.inheritable(b)), (Ok(true), Ok(false)));
        assert_eq!(t.set_inheritable(b, true), Ok(()));
        assert_eq!((table.inheritable(a), table.inheritable(b)), (Ok(true), Ok(true)));
        assert_eq!(t.set_inheritable(a, false), Ok(()));
        assert_eq!((table.inheritable(a), table.inheritable(b)), (Ok(false), Ok(true)), "a flag is the descriptor's, not its object's");
        assert_eq!(w(t.close(a)), Ok(None), "released while a dup named it");
        assert_eq!(w(t.close(b)), Ok(Some(7)));
        assert_eq!(w(t.close(b)), Err(Error::BadFd.into()));
        assert_eq!(held(&table), 0, "an object outlived its descriptors");
    }

    /// A call holds its object: a close meanwhile makes the descriptor gone
    /// at once, its number free, and the object goes when the call is done.
    #[test]
    fn a_close_during_a_call_releases_the_object_when_the_call_is_done() {
        let table = T::new();
        let mut t = writer(&table);
        let fd = t.install(o(9), false, 0).unwrap();
        let (token, object) = acquire(&table, fd).unwrap();
        assert_eq!(object.word, 9);
        assert_eq!(w(t.close(fd)), Ok(None), "the object went under a call");
        assert!(matches!(acquire(&table, fd), Err(Fault::Refused(Error::BadFd))), "a closed descriptor still answered");
        assert_eq!(t.install(o(10), false, 0), Ok(fd), "a closed number waited for a call");
        let (other, object) = acquire(&table, fd).unwrap();
        assert_eq!(object.word, 10);
        assert_ne!(other, token, "an object took the slot of one a call still holds");
        // SAFETY: each a token `acquire` answered, given back once.
        unsafe {
            assert_eq!(w(table.unuse(token)), Ok(Some(9)));
            assert_eq!(w(table.unuse(other)), Ok(None));
        }
        assert_eq!(w(t.close(fd)), Ok(Some(10)));
        assert_eq!(held(&table), 0, "an object outlived its call");
    }

    #[test]
    fn dup2_displaces_what_the_target_named_and_onto_itself_changes_nothing() {
        let table = T::new();
        let mut t = writer(&table);
        let a = t.install(o(1), false, 0).unwrap();
        let b = t.install(o(2), false, 0).unwrap();
        assert_eq!(w(t.dup2(a, b, true)), Ok(Some(2)));
        assert_eq!(table.inheritable(b), Ok(true));
        assert_eq!(w(t.dup2(a, a, true)), Ok(None));
        assert_eq!(table.inheritable(a), Ok(false), "dup2 onto itself set a flag");
        assert_eq!(w(t.dup2(a, 6, false)), Ok(None), "an unused target");
        assert_eq!(w(t.dup2(a, 8, false)), Err(Error::BadFd.into()), "a target past the table");
        assert_eq!(w(t.dup2(7, 1, false)), Err(Error::BadFd.into()));
        for fd in [a, b, 6] {
            let last = t.close(fd).unwrap();
            assert_eq!(last.is_some(), fd == 6, "descriptor {fd}");
        }
        assert_eq!(held(&table), 0, "an object outlived its descriptors");
    }

    #[test]
    fn replace_keeps_the_number_and_the_flag_and_releases_the_old_object() {
        let table = T::new();
        let mut t = writer(&table);
        let fd = t.install(o(1), true, 0).unwrap();
        assert_eq!(w(t.replace(fd, o(2))), Ok(Some(1)));
        assert_eq!(table.inheritable(fd), Ok(true));
        assert_eq!(w(t.close(fd)), Ok(Some(2)));
        assert_eq!(w(t.replace(fd, o(3))), Err(Error::BadFd.into()));
    }

    /// An endowment goes at its own number or nowhere: one whose number is
    /// open already is refused, and the table is unchanged.
    #[test]
    fn an_endowment_is_installed_at_its_number_or_refused() {
        let table = T::new();
        let mut t = writer(&table);
        assert_eq!(t.install_at(3, o(1), true), Ok(()));
        assert_eq!(table.inheritable(3), Ok(true));
        assert_eq!(t.install_at(3, o(2), true), Err(Error::BadFd.into()));
        assert_eq!(t.install_at(8, o(2), true), Err(Error::BadFd.into()));
        assert_eq!(t.install_at(-1, o(2), true), Err(Error::BadFd.into()));
        assert_eq!(w(t.close(3)), Ok(Some(1)));
        assert_eq!(t.install(o(3), false, 0), Ok(0), "a refusal took a number or an object");
    }

    #[test]
    fn endowed_aliases_share_one_object_and_cannot_displace_an_endowment() {
        let table = T::new();
        let mut t = writer(&table);
        t.install_at(3, o(1), true).unwrap();
        t.alias_at(3, 0).unwrap();
        assert_eq!(held(&table), 1);
        assert_eq!(t.alias_at(3, 0), Err(Error::BadFd.into()));
        assert_eq!(w(t.close(3)), Ok(None));
        assert_eq!(w(t.close(0)), Ok(Some(1)));
    }

    /// A descriptor naming no slot, or a free one under the writer, is a
    /// broken table, which the table answers rather than panics on.
    #[test]
    fn an_inconsistent_table_is_answered_not_panicked_on() {
        let table = T::new();
        let mut t = writer(&table);
        table.fds[2].store(9, Ordering::Relaxed);
        assert_eq!(acquire(&table, 2).map(|_| ()), Err(Fault::Broken));
        assert_eq!(w(t.close(2)), Err(Fault::Broken));
        table.fds[2].store(INHERITABLE, Ordering::Relaxed);
        assert_eq!(table.inheritable(2), Err(Fault::Broken));
        table.fds[2].store(1, Ordering::Relaxed);
        assert_eq!(w(t.close(2)), Err(Fault::Broken));
        assert_eq!(t.dup(2, 0, false), Err(Fault::Broken));
    }
}
