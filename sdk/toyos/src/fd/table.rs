//! The table's state, without its lock: every decision the table makes,
//! which a host tests. Nothing in it panics.

use super::{Error, Fd, Record, MAX_FDS};

/// A descriptor's word: 0 for a free number, else its object's index plus
/// one, with [`INHERITABLE`] set for an inheritable one. The empty table is
/// all zeros, so it costs a program no initialised data.
type Word = u16;
const INHERITABLE: Word = 0x8000;
const _: () = assert!(MAX_FDS < INHERITABLE as usize);

#[derive(Clone, Copy)]
struct Held {
    object: Record,
    /// The descriptors naming it and the calls holding it; 0 for a free one.
    refs: u32,
}

const FREE: Held = Held { object: Record::FREE, refs: 0 };

/// Why a table operation failed: refused, or the table found inconsistent,
/// which the owner panics on only once its lock is released, since the panic
/// writes to a descriptor.
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

/// A removal answers the object it released, for the caller to let go
/// outside the lock.
pub(super) struct Table<const N: usize> {
    fds: [Word; N],
    objects: [Held; N],
}

impl<const N: usize> Table<N> {
    pub(super) const fn new() -> Self {
        Self { fds: [0; N], objects: [FREE; N] }
    }

    /// `fd`'s index, object and flag.
    fn entry(&self, fd: Fd) -> Result<(usize, usize, bool), Fault> {
        let i = usize::try_from(fd).map_err(|_| Error::BadFd)?;
        let word = *self.fds.get(i).ok_or(Error::BadFd)?;
        let object = usize::from(word & !INHERITABLE).checked_sub(1).ok_or(match word {
            0 => Fault::Refused(Error::BadFd),
            _ => Fault::Broken,
        })?;
        Ok((i, object, word & INHERITABLE != 0))
    }

    fn set(&mut self, i: usize, object: usize, inheritable: bool) -> Result<(), Fault> {
        let index = Word::try_from(object).ok().filter(|&o| o < INHERITABLE - 1).ok_or(Fault::Broken)?;
        let flag = if inheritable { INHERITABLE } else { 0 };
        *self.fds.get_mut(i).ok_or(Fault::Broken)? = (index + 1) | flag;
        Ok(())
    }

    fn lowest_free(&self, floor: Fd) -> Result<usize, Error> {
        let floor = usize::try_from(floor).map_err(|_| Error::Full)?;
        let above = self.fds.get(floor..).ok_or(Error::Full)?;
        above.iter().position(|&w| w == 0).map(|i| floor + i).ok_or(Error::Full)
    }

    /// A free object holding `object`, referenced once.
    fn hold(&mut self, object: Record) -> Result<usize, Error> {
        let slot = self.objects.iter().position(|h| h.refs == 0).ok_or(Error::Full)?;
        self.objects[slot] = Held { object, refs: 1 };
        Ok(slot)
    }

    fn held(&mut self, object: usize) -> Result<&mut Held, Fault> {
        self.objects.get_mut(object).filter(|h| h.refs != 0).ok_or(Fault::Broken)
    }

    fn reference(&mut self, object: usize) -> Result<Record, Fault> {
        let held = self.held(object)?;
        held.refs = held.refs.checked_add(1).ok_or(Fault::Broken)?;
        Ok(held.object)
    }

    /// A reference fewer on `object`: the object, once none is left.
    fn unref(&mut self, object: usize) -> Result<Option<Record>, Fault> {
        let held = self.held(object)?;
        held.refs -= 1;
        Ok((held.refs == 0).then(|| core::mem::replace(held, FREE).object))
    }

    /// Name `object` by the lowest free descriptor at or above `floor`.
    pub(super) fn install(&mut self, object: Record, inheritable: bool, floor: Fd) -> Result<Fd, Fault> {
        let fd = self.lowest_free(floor)?;
        let slot = self.hold(object)?;
        self.set(fd, slot, inheritable)?;
        Ok(fd as Fd)
    }

    /// Name `object` by `fd` exactly: `BadFd` for a number past the table
    /// or already open.
    pub(super) fn install_at(&mut self, fd: Fd, object: Record, inheritable: bool) -> Result<(), Fault> {
        let i = usize::try_from(fd).map_err(|_| Error::BadFd)?;
        if self.fds.get(i) != Some(&0) {
            return Err(Error::BadFd.into());
        }
        let slot = self.hold(object)?;
        self.set(i, slot, inheritable)
    }

    /// A second descriptor on `fd`'s object, the lowest free at or above
    /// `floor`.
    pub(super) fn dup(&mut self, fd: Fd, floor: Fd, inheritable: bool) -> Result<Fd, Fault> {
        let (_, object, _) = self.entry(fd)?;
        let new = self.lowest_free(floor)?;
        self.reference(object)?;
        self.set(new, object, inheritable)?;
        Ok(new as Fd)
    }

    /// `new` on `old`'s object, whatever `new` named before: that object,
    /// if this was its last reference. `new == old` changes nothing.
    pub(super) fn dup2(&mut self, old: Fd, new: Fd, inheritable: bool) -> Result<Option<Record>, Fault> {
        let (_, object, _) = self.entry(old)?;
        let slot = usize::try_from(new).ok().filter(|&i| i < N).ok_or(Error::BadFd)?;
        if new == old {
            return Ok(None);
        }
        let displaced = self.entry(new).ok();
        self.reference(object)?;
        self.set(slot, object, inheritable)?;
        match displaced {
            Some((_, displaced, _)) => self.unref(displaced),
            None => Ok(None),
        }
    }

    /// Forget `fd`: its object, if this was its last reference.
    pub(super) fn close(&mut self, fd: Fd) -> Result<Option<Record>, Fault> {
        let (i, object, _) = self.entry(fd)?;
        *self.fds.get_mut(i).ok_or(Fault::Broken)? = 0;
        self.unref(object)
    }

    /// `fd`'s object, held until [`Self::unuse`] of the token answered.
    pub(super) fn acquire(&mut self, fd: Fd) -> Result<(usize, Record), Fault> {
        let (_, object, _) = self.entry(fd)?;
        Ok((object, self.reference(object)?))
    }

    /// A call done with the object `token` names: the object, if nothing
    /// names it any more.
    pub(super) fn unuse(&mut self, token: usize) -> Result<Option<Record>, Fault> {
        self.unref(token)
    }

    /// Make `fd` name `object` from here on, in place: the object it named,
    /// if this was its last reference. Its flag stays.
    pub(super) fn replace(&mut self, fd: Fd, object: Record) -> Result<Option<Record>, Fault> {
        let (i, old, inheritable) = self.entry(fd)?;
        let slot = self.hold(object)?;
        self.set(i, slot, inheritable)?;
        self.unref(old)
    }

    pub(super) fn inheritable(&self, fd: Fd) -> Result<bool, Fault> {
        self.entry(fd).map(|(_, _, inheritable)| inheritable)
    }

    pub(super) fn set_inheritable(&mut self, fd: Fd, inheritable: bool) -> Result<(), Fault> {
        let (i, object, _) = self.entry(fd)?;
        self.set(i, object, inheritable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    extern "C" fn nothing(_: u64) {}

    fn o(word: u64) -> Record {
        Record { handle: toyos_abi::HANDLE_INVALID, tag: super::super::UNTAGGED, word, release: Some(nothing) }
    }

    type T = Table<8>;

    /// The word of what a removal released, which names it in these tests.
    fn w(released: Result<Option<Record>, Fault>) -> Result<Option<u64>, Fault> {
        released.map(|o| o.map(|o| o.word))
    }

    /// The objects something still references.
    fn held(t: &T) -> usize {
        t.objects.iter().filter(|h| h.refs != 0).count()
    }

    #[test]
    fn descriptors_are_numbered_lowest_free_from_the_floor() {
        let mut t = T::new();
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
        let mut t = T::new();
        let a = t.install(o(7), true, 0).unwrap();
        let b = t.dup(a, 0, false).unwrap();
        assert_eq!((t.inheritable(a), t.inheritable(b)), (Ok(true), Ok(false)));
        assert_eq!(t.set_inheritable(b, true), Ok(()));
        assert_eq!((t.inheritable(a), t.inheritable(b)), (Ok(true), Ok(true)));
        assert_eq!(t.set_inheritable(a, false), Ok(()));
        assert_eq!((t.inheritable(a), t.inheritable(b)), (Ok(false), Ok(true)), "a flag is the descriptor's, not its object's");
        assert_eq!(w(t.close(a)), Ok(None), "released while a dup named it");
        assert_eq!(w(t.close(b)), Ok(Some(7)));
        assert_eq!(w(t.close(b)), Err(Error::BadFd.into()));
        assert_eq!(held(&t), 0, "an object outlived its descriptors");
    }

    /// A call holds its object: a close meanwhile makes the descriptor gone
    /// at once, and the object goes when the call is done.
    #[test]
    fn a_close_during_a_call_releases_the_object_when_the_call_is_done() {
        let mut t = T::new();
        let fd = t.install(o(9), false, 0).unwrap();
        let (token, object) = t.acquire(fd).unwrap();
        assert_eq!(object.word, 9);
        assert_eq!(w(t.close(fd)), Ok(None), "the object went under a call");
        assert!(matches!(t.acquire(fd), Err(Fault::Refused(Error::BadFd))), "a closed descriptor still answered");
        assert_eq!(w(t.unuse(token)), Ok(Some(9)));
        assert_eq!(held(&t), 0, "an object outlived its call");
    }

    #[test]
    fn dup2_displaces_what_the_target_named_and_onto_itself_changes_nothing() {
        let mut t = T::new();
        let a = t.install(o(1), false, 0).unwrap();
        let b = t.install(o(2), false, 0).unwrap();
        assert_eq!(w(t.dup2(a, b, true)), Ok(Some(2)));
        assert_eq!(t.inheritable(b), Ok(true));
        assert_eq!(w(t.dup2(a, a, true)), Ok(None));
        assert_eq!(t.inheritable(a), Ok(false), "dup2 onto itself set a flag");
        assert_eq!(w(t.dup2(a, 6, false)), Ok(None), "an unused target");
        assert_eq!(w(t.dup2(a, 8, false)), Err(Error::BadFd.into()), "a target past the table");
        assert_eq!(w(t.dup2(7, 1, false)), Err(Error::BadFd.into()));
        for fd in [a, b, 6] {
            let last = t.close(fd).unwrap();
            assert_eq!(last.is_some(), fd == 6, "descriptor {fd}");
        }
        assert_eq!(held(&t), 0, "an object outlived its descriptors");
    }

    #[test]
    fn replace_keeps_the_number_and_the_flag_and_releases_the_old_object() {
        let mut t = T::new();
        let fd = t.install(o(1), true, 0).unwrap();
        assert_eq!(w(t.replace(fd, o(2))), Ok(Some(1)));
        assert_eq!(t.inheritable(fd), Ok(true));
        assert_eq!(w(t.close(fd)), Ok(Some(2)));
        assert_eq!(w(t.replace(fd, o(3))), Err(Error::BadFd.into()));
    }

    /// An endowment goes at its own number or nowhere: one whose number is
    /// open already is refused, and the table is unchanged.
    #[test]
    fn an_endowment_is_installed_at_its_number_or_refused() {
        let mut t = T::new();
        assert_eq!(t.install_at(3, o(1), true), Ok(()));
        assert_eq!(t.inheritable(3), Ok(true));
        assert_eq!(t.install_at(3, o(2), true), Err(Error::BadFd.into()));
        assert_eq!(t.install_at(8, o(2), true), Err(Error::BadFd.into()));
        assert_eq!(t.install_at(-1, o(2), true), Err(Error::BadFd.into()));
        assert_eq!(w(t.close(3)), Ok(Some(1)));
        assert_eq!(t.install(o(3), false, 0), Ok(0), "a refusal took a number or an object");
    }

    /// A descriptor naming no object is a broken table, which the table
    /// answers rather than panics on: the owner panics once its lock is let
    /// go.
    #[test]
    fn an_inconsistent_table_is_answered_not_panicked_on() {
        let mut t = T::new();
        t.fds[2] = 1;
        assert_eq!(t.acquire(2).map(|_| ()), Err(Fault::Broken));
        assert_eq!(w(t.close(2)), Err(Fault::Broken));
        t.fds[2] = INHERITABLE;
        assert_eq!(t.inheritable(2), Err(Fault::Broken));
    }
}
