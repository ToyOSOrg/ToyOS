//! The process's one descriptor table: what a raw fd number means, for std,
//! libc and every crate between them.
//!
//! **A descriptor names an object**: a kernel handle, or an object of a
//! userland library that owns it ([`Object::opaque`]). Descriptors are
//! numbered lowest-free as POSIX has them, and [`dup`] and [`dup2`] make
//! further descriptors that share one object. Each descriptor carries its own
//! **inheritable** flag; whether a child is given it is the launchers' rule.
//!
//! **A call on a descriptor holds its object for the call** ([`with`]): a
//! descriptor closed meanwhile goes at once, and its object is released when
//! the last descriptor and the last call are done. So a racing close makes
//! the next lookup `BadFd`, and never a call on a handle that was closed.
//!
//! **One copy of this crate owns the table**, the one the `fd-table` feature
//! is on in: std's (`rustc-dep-of-std`) in a Rust program, libc's own in a C
//! program. It exports `toyos_fd_*`, and every copy, the owner's included,
//! reaches the table only through them, so all copies linked into one image
//! share one table. Nothing catches a second owner at link time: the feature
//! is set in those two builds and nowhere else.
//!
//! **A process's descriptors arrive as endowments**, `fd:<n>` (`FD_PREFIX`),
//! each installed at its number at the table's first use; its stdio is
//! descriptors 0 to 2 among them.

use toyos_abi::RawHandle;

/// A descriptor number.
pub type Fd = i32;

/// Descriptors and objects one table holds: as many as the kernel holds
/// handles for one process.
pub const MAX_FDS: usize = 4096;

/// Why a table call was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// No descriptor by that number: `EBADF`.
    BadFd,
    /// No free descriptor at or above the floor asked, or no free object:
    /// `EMFILE`.
    Full,
}

/// What a descriptor names.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Object {
    /// The kernel handle, or [`toyos_abi::HANDLE_INVALID`] for an opaque object.
    pub handle: RawHandle,
    /// An opaque object's word, its owner's to interpret.
    pub word: u64,
    /// How an opaque object is let go once nothing names it.
    pub release: Option<extern "C" fn(u64)>,
}

impl Object {
    /// A kernel handle the table owns from here on: it is closed when the
    /// object is released.
    pub const fn handle(handle: RawHandle) -> Self {
        Self { handle, word: 0, release: None }
    }

    /// An object a library keeps itself, `word`, which `release` is called
    /// with once no descriptor and no call names it.
    pub const fn opaque(word: u64, release: extern "C" fn(u64)) -> Self {
        Self { handle: toyos_abi::HANDLE_INVALID, word, release: Some(release) }
    }

    /// Let the object go: an opaque one to its library, a handle closed (a
    /// stream's end with its last handle, `net::stream::let_go`).
    pub fn release(self) {
        match self.release {
            Some(release) => release(self.word),
            None => crate::net::stream::let_go(self.handle),
        }
    }
}

/// One descriptor's entry: its object's index and its own flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Entry {
    object: u16,
    inheritable: bool,
}

#[derive(Clone, Copy)]
struct Held {
    object: Object,
    /// The descriptors naming it and the calls holding it.
    refs: u32,
}

/// The table's state, without its lock: every decision the table makes,
/// which a host tests. A removal answers the object it released, for the
/// caller to let go outside the lock.
pub struct Table<const N: usize> {
    fds: [Option<Entry>; N],
    objects: [Option<Held>; N],
}

impl<const N: usize> Default for Table<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Table<N> {
    pub const fn new() -> Self {
        Self { fds: [None; N], objects: [None; N] }
    }

    fn entry(&self, fd: Fd) -> Result<Entry, Error> {
        usize::try_from(fd).ok().and_then(|i| self.fds.get(i).copied().flatten()).ok_or(Error::BadFd)
    }

    fn lowest_free(&self, floor: Fd) -> Result<usize, Error> {
        let floor = usize::try_from(floor).map_err(|_| Error::Full)?;
        (floor..N).find(|&i| self.fds[i].is_none()).ok_or(Error::Full)
    }

    /// A reference fewer on `object`: the object, once none is left.
    fn unref(&mut self, object: u16) -> Option<Object> {
        let held = self.objects[object as usize].as_mut().expect("a referenced object");
        held.refs -= 1;
        match held.refs {
            0 => self.objects[object as usize].take().map(|h| h.object),
            _ => None,
        }
    }

    /// Name `object` by the lowest free descriptor at or above `floor`.
    pub fn install(&mut self, object: Object, inheritable: bool, floor: Fd) -> Result<Fd, Error> {
        let fd = self.lowest_free(floor)?;
        let slot = self.objects.iter().position(Option::is_none).ok_or(Error::Full)?;
        self.objects[slot] = Some(Held { object, refs: 1 });
        self.fds[fd] = Some(Entry { object: slot as u16, inheritable });
        Ok(fd as Fd)
    }

    /// A second descriptor on `fd`'s object, the lowest free at or above
    /// `floor`.
    pub fn dup(&mut self, fd: Fd, floor: Fd, inheritable: bool) -> Result<Fd, Error> {
        let entry = self.entry(fd)?;
        let new = self.lowest_free(floor)?;
        self.objects[entry.object as usize].as_mut().expect("named").refs += 1;
        self.fds[new] = Some(Entry { object: entry.object, inheritable });
        Ok(new as Fd)
    }

    /// `new` on `old`'s object, whatever `new` named before: that object,
    /// if this was its last reference. `new == old` changes nothing.
    pub fn dup2(&mut self, old: Fd, new: Fd, inheritable: bool) -> Result<Option<Object>, Error> {
        let entry = self.entry(old)?;
        let slot = usize::try_from(new).ok().filter(|&i| i < N).ok_or(Error::BadFd)?;
        if new == old {
            return Ok(None);
        }
        self.objects[entry.object as usize].as_mut().expect("named").refs += 1;
        let displaced = self.fds[slot].replace(Entry { object: entry.object, inheritable });
        Ok(displaced.and_then(|d| self.unref(d.object)))
    }

    /// Forget `fd`: its object, if this was its last reference.
    pub fn close(&mut self, fd: Fd) -> Result<Option<Object>, Error> {
        let entry = self.entry(fd)?;
        self.fds[fd as usize] = None;
        Ok(self.unref(entry.object))
    }

    /// `fd`'s object, held until [`Self::unuse`] of the token answered.
    pub fn acquire(&mut self, fd: Fd) -> Result<(u16, Object), Error> {
        let entry = self.entry(fd)?;
        let held = self.objects[entry.object as usize].as_mut().expect("named");
        held.refs += 1;
        Ok((entry.object, held.object))
    }

    /// A call done with the object `token` names: the object, if nothing
    /// names it any more.
    pub fn unuse(&mut self, token: u16) -> Option<Object> {
        self.unref(token)
    }

    /// Make `fd` name `object` from here on, in place: the object it named,
    /// if this was its last reference. Its flag stays.
    pub fn replace(&mut self, fd: Fd, object: Object) -> Result<Option<Object>, Error> {
        let entry = self.entry(fd)?;
        let slot = self.objects.iter().position(Option::is_none).ok_or(Error::Full)?;
        self.objects[slot] = Some(Held { object, refs: 1 });
        self.fds[fd as usize] = Some(Entry { object: slot as u16, inheritable: entry.inheritable });
        Ok(self.unref(entry.object))
    }

    pub fn inheritable(&self, fd: Fd) -> Result<bool, Error> {
        self.entry(fd).map(|e| e.inheritable)
    }

    pub fn set_inheritable(&mut self, fd: Fd, inheritable: bool) -> Result<(), Error> {
        self.entry(fd)?;
        self.fds[fd as usize].as_mut().expect("present").inheritable = inheritable;
        Ok(())
    }
}

// The C interface: what every copy calls and the owner defines. A negative
// answer is an [`Error`]: -1 `BadFd`, -2 `Full`.

const BAD_FD: i32 = -1;
const FULL: i32 = -2;

#[cfg(any(feature = "fd-table", test))]
fn encode(e: Error) -> i32 {
    match e {
        Error::BadFd => BAD_FD,
        Error::Full => FULL,
    }
}

fn decode(word: i32) -> Result<i32, Error> {
    match word {
        BAD_FD => Err(Error::BadFd),
        FULL => Err(Error::Full),
        n => Ok(n),
    }
}

extern "C" {
    fn toyos_fd_install(object: Object, inheritable: bool, floor: Fd) -> i32;
    fn toyos_fd_acquire(fd: Fd, out: *mut Object) -> i32;
    fn toyos_fd_unuse(token: i32);
    fn toyos_fd_close(fd: Fd) -> i32;
    fn toyos_fd_dup(fd: Fd, floor: Fd, inheritable: bool) -> i32;
    fn toyos_fd_dup2(old: Fd, new: Fd, inheritable: bool) -> i32;
    fn toyos_fd_replace(fd: Fd, object: Object) -> i32;
    fn toyos_fd_inheritable(fd: Fd) -> i32;
    fn toyos_fd_set_inheritable(fd: Fd, inheritable: bool) -> i32;
}

/// Name `object` by the lowest free descriptor at or above `floor`. On
/// `Full` the object is let go, so nothing it held is left behind.
pub fn install(object: Object, inheritable: bool, floor: Fd) -> Result<Fd, Error> {
    // SAFETY: the owner's definition below, which takes the object by value.
    decode(unsafe { toyos_fd_install(object, inheritable, floor) })
}

/// `f` on `fd`'s object, held for the call: `BadFd` for a descriptor not
/// open.
pub fn with<R>(fd: Fd, f: impl FnOnce(Object) -> R) -> Result<R, Error> {
    let mut object = Object::handle(toyos_abi::HANDLE_INVALID);
    // SAFETY: `object` is a live place for the answer.
    let token = decode(unsafe { toyos_fd_acquire(fd, &mut object) })?;
    struct Unuse(i32);
    impl Drop for Unuse {
        fn drop(&mut self) {
            // SAFETY: a token `toyos_fd_acquire` answered, given back once.
            unsafe { toyos_fd_unuse(self.0) }
        }
    }
    let _unuse = Unuse(token);
    Ok(f(object))
}

pub fn close(fd: Fd) -> Result<(), Error> {
    // SAFETY: plain values.
    decode(unsafe { toyos_fd_close(fd) }).map(drop)
}

pub fn dup(fd: Fd, floor: Fd, inheritable: bool) -> Result<Fd, Error> {
    // SAFETY: plain values.
    decode(unsafe { toyos_fd_dup(fd, floor, inheritable) })
}

/// POSIX's `dup2`: `new` on `old`'s object, after closing what `new` named.
pub fn dup2(old: Fd, new: Fd, inheritable: bool) -> Result<Fd, Error> {
    // SAFETY: plain values.
    decode(unsafe { toyos_fd_dup2(old, new, inheritable) })
}

/// Make `fd` name `object` in place, letting go what it named: a socket
/// that connects becomes its stream at the same number. On `BadFd` or
/// `Full` the object is let go.
pub fn replace(fd: Fd, object: Object) -> Result<(), Error> {
    // SAFETY: the owner's definition below, which takes the object by value.
    decode(unsafe { toyos_fd_replace(fd, object) }).map(drop)
}

/// A copy of the kernel handle `fd` names, for a child: `None` for a
/// descriptor that is not open, as a child of POSIX's holds no closed one;
/// `Err` for one that names no kernel handle, or that the kernel will not
/// duplicate.
pub fn copy(fd: Fd) -> Result<Option<crate::OwnedHandle>, toyos_abi::syscall::SyscallError> {
    match with(fd, |o| match o.release {
        None => toyos_abi::syscall::dup(o.handle).map(crate::OwnedHandle),
        Some(_) => Err(toyos_abi::syscall::SyscallError::InvalidArgument),
    }) {
        Ok(copied) => copied.map(Some),
        Err(_) => Ok(None),
    }
}

pub fn inheritable(fd: Fd) -> Result<bool, Error> {
    // SAFETY: plain values.
    decode(unsafe { toyos_fd_inheritable(fd) }).map(|flag| flag != 0)
}

pub fn set_inheritable(fd: Fd, inheritable: bool) -> Result<(), Error> {
    // SAFETY: plain values.
    decode(unsafe { toyos_fd_set_inheritable(fd, inheritable) }).map(drop)
}

/// The descriptor an `fd:<n>` endowment's label names: `None` for any other
/// label, and for a number past the table.
pub fn endowed_fd(label: &str) -> Option<Fd> {
    let n: Fd = label.strip_prefix(toyos_abi::syscall::FD_PREFIX)?.parse().ok()?;
    (0..MAX_FDS as Fd).contains(&n).then_some(n)
}

/// The table itself, in the copy that owns it.
#[cfg(all(feature = "fd-table", not(test)))]
mod owner {
    use super::*;
    use crate::endow::Endowments;
    use crate::sync::Lock;
    use crate::OwnedHandle;

    /// The table, and whether its endowed descriptors were installed yet.
    static TABLE: Lock<(bool, Table<MAX_FDS>)> = Lock::new((false, Table::new()));

    /// `f` on the table, whose first use installs every descriptor the
    /// process was endowed at its number.
    fn table<R>(f: impl FnOnce(&mut Table<MAX_FDS>) -> R) -> R {
        let mut held = TABLE.lock();
        let (ready, table) = &mut *held;
        if !*ready {
            *ready = true;
            let endowed = Endowments::get();
            for label in endowed.labels() {
                let Some(fd) = endowed_fd(label) else { continue };
                let Some(handle) = endowed.take::<OwnedHandle>(label) else { continue };
                let installed = table.install(Object::handle(handle.into_raw()), true, fd);
                assert_eq!(installed, Ok(fd), "toyos::fd: endowed descriptor {fd} is not free at its number");
            }
        }
        f(table)
    }

    fn released(object: Option<Object>) {
        if let Some(object) = object {
            object.release();
        }
    }

    #[no_mangle]
    extern "C" fn toyos_fd_install(object: Object, inheritable: bool, floor: Fd) -> i32 {
        table(|t| t.install(object, inheritable, floor)).unwrap_or_else(|e| {
            object.release();
            encode(e)
        })
    }

    #[no_mangle]
    unsafe extern "C" fn toyos_fd_acquire(fd: Fd, out: *mut Object) -> i32 {
        match table(|t| t.acquire(fd)) {
            Ok((token, object)) => {
                // SAFETY: the caller's live place.
                unsafe { out.write(object) };
                i32::from(token)
            }
            Err(e) => encode(e),
        }
    }

    #[no_mangle]
    extern "C" fn toyos_fd_unuse(token: i32) {
        let token = u16::try_from(token).expect("toyos::fd: a token no acquire answered");
        released(table(|t| t.unuse(token)));
    }

    #[no_mangle]
    extern "C" fn toyos_fd_close(fd: Fd) -> i32 {
        match table(|t| t.close(fd)) {
            Ok(object) => {
                released(object);
                0
            }
            Err(e) => encode(e),
        }
    }

    #[no_mangle]
    extern "C" fn toyos_fd_dup(fd: Fd, floor: Fd, inheritable: bool) -> i32 {
        table(|t| t.dup(fd, floor, inheritable)).unwrap_or_else(encode)
    }

    #[no_mangle]
    extern "C" fn toyos_fd_dup2(old: Fd, new: Fd, inheritable: bool) -> i32 {
        match table(|t| t.dup2(old, new, inheritable)) {
            Ok(displaced) => {
                released(displaced);
                new
            }
            Err(e) => encode(e),
        }
    }

    #[no_mangle]
    extern "C" fn toyos_fd_replace(fd: Fd, object: Object) -> i32 {
        match table(|t| t.replace(fd, object)) {
            Ok(old) => {
                released(old);
                0
            }
            Err(e) => {
                object.release();
                encode(e)
            }
        }
    }

    #[no_mangle]
    extern "C" fn toyos_fd_inheritable(fd: Fd) -> i32 {
        table(|t| t.inheritable(fd)).map_or_else(encode, i32::from)
    }

    #[no_mangle]
    extern "C" fn toyos_fd_set_inheritable(fd: Fd, inheritable: bool) -> i32 {
        table(|t| t.set_inheritable(fd, inheritable)).map_or_else(encode, |()| 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    extern "C" fn nothing(_: u64) {}

    fn o(word: u64) -> Object {
        Object::opaque(word, nothing)
    }

    type T = Table<8>;

    /// The word of what a removal released, which names it in these tests.
    fn w(released: Result<Option<Object>, Error>) -> Result<Option<u64>, Error> {
        released.map(|o| o.map(|o| o.word))
    }

    #[test]
    fn descriptors_are_numbered_lowest_free_from_the_floor() {
        let mut t = T::new();
        assert_eq!(t.install(o(1), false, 0), Ok(0));
        assert_eq!(t.install(o(2), false, 0), Ok(1));
        assert_eq!(t.install(o(3), false, 5), Ok(5));
        assert_eq!(w(t.close(0)), Ok(Some(1)));
        assert_eq!(t.install(o(4), false, 0), Ok(0), "a closed number is the lowest free again");
        assert_eq!(t.install(o(5), false, 8), Err(Error::Full));
        assert_eq!(t.install(o(5), false, -1), Err(Error::Full));
    }

    /// Descriptors on one object release it with the last of them, and each
    /// keeps a flag of its own.
    #[test]
    fn an_object_goes_with_its_last_descriptor_and_flags_are_per_descriptor() {
        let mut t = T::new();
        let a = t.install(o(7), true, 0).unwrap();
        let b = t.dup(a, 0, false).unwrap();
        assert_eq!((t.inheritable(a), t.inheritable(b)), (Ok(true), Ok(false)));
        assert_eq!(w(t.close(a)), Ok(None), "released while a dup named it");
        assert_eq!(w(t.close(b)), Ok(Some(7)));
        assert_eq!(w(t.close(b)), Err(Error::BadFd));
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
        assert!(matches!(t.acquire(fd), Err(Error::BadFd)), "a closed descriptor still answered");
        assert_eq!(t.unuse(token).map(|o| o.word), Some(9));
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
        assert_eq!(w(t.dup2(a, 8, false)), Err(Error::BadFd), "a target past the table");
        assert_eq!(w(t.dup2(7, 1, false)), Err(Error::BadFd));
        for fd in [a, b, 6] {
            let last = t.close(fd).unwrap();
            assert_eq!(last.is_some(), fd == 6, "descriptor {fd}");
        }
    }

    #[test]
    fn replace_keeps_the_number_and_the_flag_and_releases_the_old_object() {
        let mut t = T::new();
        let fd = t.install(o(1), true, 0).unwrap();
        assert_eq!(w(t.replace(fd, o(2))), Ok(Some(1)));
        assert_eq!(t.inheritable(fd), Ok(true));
        assert_eq!(w(t.close(fd)), Ok(Some(2)));
        assert_eq!(w(t.replace(fd, o(3))), Err(Error::BadFd));
    }

    #[test]
    fn an_fd_label_names_its_number_and_no_other_label_names_one() {
        assert_eq!(endowed_fd("fd:0"), Some(0));
        assert_eq!(endowed_fd("fd:4095"), Some(4095));
        for other in ["fd:4096", "fd:-1", "fd:", "fd:1x", "svc", "fd1", "dev:fd:1"] {
            assert_eq!(endowed_fd(other), None, "{other}");
        }
    }

    #[test]
    fn the_c_words_round_trip_every_refusal() {
        for e in [Error::BadFd, Error::Full] {
            assert_eq!(decode(encode(e)), Err(e));
        }
        assert_eq!(decode(0), Ok(0));
        assert_eq!(decode(4095), Ok(4095));
    }
}
