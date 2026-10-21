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

/// The tag of a handle and of an object another library keeps.
pub const UNTAGGED: u32 = 0;
/// The tag of a file a file server holds open (`crate::files`).
pub const SERVED_FILE: u32 = 1;

/// What a descriptor names.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Object {
    /// The kernel handle, or [`toyos_abi::HANDLE_INVALID`] for an opaque object.
    pub handle: RawHandle,
    /// Which kind of opaque object this crate keeps it is, [`UNTAGGED`] for a
    /// handle and for another library's.
    pub tag: u32,
    /// An opaque object's word, its owner's to interpret.
    pub word: u64,
    /// How an opaque object is let go once nothing names it.
    pub release: Option<extern "C" fn(u64)>,
}

impl Object {
    /// A kernel handle the table owns from here on: it is closed when the
    /// object is released.
    pub const fn handle(handle: RawHandle) -> Self {
        Self { handle, tag: UNTAGGED, word: 0, release: None }
    }

    /// An object a library keeps itself, `word`, which `release` is called
    /// with once no descriptor and no call names it.
    pub const fn opaque(word: u64, release: extern "C" fn(u64)) -> Self {
        Self::tagged(UNTAGGED, word, release)
    }

    /// An opaque object of a kind this crate keeps, which [`Object::tag`] names.
    pub(crate) const fn tagged(tag: u32, word: u64, release: extern "C" fn(u64)) -> Self {
        Self { handle: toyos_abi::HANDLE_INVALID, tag, word, release: Some(release) }
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

#[cfg(any(feature = "fd-table", test))]
mod table;

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

/// POSIX's `dup2`: `new` on `old`'s object, after closing what `new` named;
/// onto itself, nothing but `old` checked open.
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
/// descriptor that is not open, as a child of POSIX's holds no closed one.
/// Refused for a stream's end, which `net::stream` answers for here alone,
/// for an object that is no kernel handle, and for one the kernel will not
/// duplicate.
pub fn copy(fd: Fd) -> Result<Option<crate::OwnedHandle>, crate::spawn::Refused> {
    use crate::spawn::Refused;
    let copied = with(fd, |o| match o.release {
        None if crate::net::stream::is_stream(o.handle) => Err(Refused::Stream(o.handle)),
        None => toyos_abi::syscall::dup(o.handle).map(crate::OwnedHandle).map_err(Refused::Syscall),
        Some(_) => Err(Refused::Syscall(toyos_abi::syscall::SyscallError::NotSupported)),
    });
    copied.map_or(Ok(None), |copied| copied.map(Some))
}

pub fn inheritable(fd: Fd) -> Result<bool, Error> {
    // SAFETY: plain values.
    decode(unsafe { toyos_fd_inheritable(fd) }).map(|flag| flag != 0)
}

pub fn set_inheritable(fd: Fd, inheritable: bool) -> Result<(), Error> {
    // SAFETY: plain values.
    decode(unsafe { toyos_fd_set_inheritable(fd, inheritable) }).map(drop)
}

/// The descriptor an `fd:<n>` endowment's label names, `n` in canonical
/// decimal: `None` for any other label, and for a number past the table.
pub fn endowed_fd(label: &str) -> Option<Fd> {
    let digits = label.strip_prefix(toyos_abi::syscall::FD_PREFIX)?;
    let canonical = digits.bytes().all(|b| b.is_ascii_digit()) && (digits == "0" || !digits.starts_with('0'));
    let n: Fd = digits.parse().ok().filter(|_| canonical)?;
    (0..MAX_FDS as Fd).contains(&n).then_some(n)
}

/// The table itself, in the copy that owns it.
#[cfg(all(feature = "fd-table", not(test)))]
mod owner {
    use super::table::{Fault, Table};
    use super::*;
    use crate::endow::Endowments;
    use crate::sync::Lock;
    use crate::OwnedHandle;

    /// The table, and whether its endowed descriptors were installed yet.
    static TABLE: Lock<(bool, Table<MAX_FDS>)> = Lock::new((false, Table::new()));

    /// `f` on the table, whose first use installs every descriptor the
    /// process was endowed at its number. Nothing panics under the lock: a
    /// panic writes to stderr, a descriptor, which would wait on it forever.
    fn table<R>(f: impl FnOnce(&mut Table<MAX_FDS>) -> Result<R, Fault>) -> Result<R, Error> {
        let endowed = Endowments::get();
        let mut refused = 0;
        let answer = {
            let mut held = TABLE.lock();
            let (ready, table) = &mut *held;
            let installed = match *ready {
                true => Ok(()),
                false => {
                    *ready = true;
                    endow(table, endowed).map(|n| refused = n)
                }
            };
            installed.and_then(|()| f(table))
        };
        // Said once the lock is let go, since stderr is a descriptor.
        for _ in 0..refused {
            let _ = crate::files::write(2, b"toyos::fd: an fd: endowment whose number was taken was refused\n");
        }
        answer.map_err(|fault| match fault {
            Fault::Refused(e) => e,
            Fault::Broken => panic!("toyos::fd: the descriptor table is inconsistent"),
        })
    }

    /// Install each `fd:<n>` endowment at `n`. One whose number another
    /// already took is refused, its handle closed: how many were.
    fn endow(table: &mut Table<MAX_FDS>, endowed: &Endowments) -> Result<usize, Fault> {
        let mut refused = 0;
        for label in endowed.labels() {
            let Some(fd) = endowed_fd(label) else { continue };
            let Some(handle) = endowed.take::<OwnedHandle>(label) else { continue };
            match table.install_at(fd, Object::handle(handle.0), true) {
                // The table owns the handle from here on.
                Ok(()) => drop(handle.into_raw()),
                Err(Fault::Refused(_)) => {
                    drop(handle);
                    refused += 1;
                }
                Err(Fault::Broken) => return Err(Fault::Broken),
            }
        }
        Ok(refused)
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
                token as i32
            }
            Err(e) => encode(e),
        }
    }

    #[no_mangle]
    extern "C" fn toyos_fd_unuse(token: i32) {
        let token = usize::try_from(token).expect("toyos::fd: a token no acquire answered");
        released(table(|t| t.unuse(token)).expect("toyos::fd: a token no acquire answered"));
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

    #[test]
    fn an_fd_label_names_its_number_and_no_other_label_names_one() {
        assert_eq!(endowed_fd("fd:0"), Some(0));
        assert_eq!(endowed_fd("fd:4095"), Some(4095));
        for other in ["fd:4096", "fd:-1", "fd:", "fd:1x", "svc", "fd1", "dev:fd:1", "fd:+3", "fd:03", "fd:00", "fd: 3"] {
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
