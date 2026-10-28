//! The process's one descriptor table: what a raw fd number means, for std,
//! libc and every crate between them.
//!
//! **A descriptor names an object**: a kernel handle, or an object of a
//! userland library that owns it ([`Object::opaque`]). Descriptors are
//! numbered lowest-free as POSIX has them, and [`dup`] and [`dup2`] make
//! further descriptors that share one object. Each descriptor carries its own
//! **inheritable** flag; whether a child is given it is the launchers' rule.
//!
//! **A call on a descriptor holds its object for the call** ([`with`],
//! [`hold`]): a descriptor closed meanwhile goes at once, and its object is
//! released when the last descriptor and the last call are done. So a racing
//! close makes the next lookup `BadFd`, and never a call on a handle that was
//! closed.
//!
//! **One copy of this crate owns the table**, the one the `fd-table` feature
//! is on in: std's (`rustc-dep-of-std`) in a Rust program, libc's own in a C
//! program. It exports `toyos_fd_*`, and every copy, the owner's included,
//! reaches the table only through them, so all copies linked into one image
//! share one table. Nothing catches a second owner at link time: the feature
//! is set in those two builds and nowhere else.
//!
//! **A process's descriptors arrive as endowments**, `fd:<n>` (`FD_PREFIX`)
//! or `fd:<n>:<kind>` ([`Kind`]), each installed at its number at the table's
//! first use; its stdio is descriptors 0 to 2 among them. A stream's, a
//! listener's and a datagram socket's arrive as an exported end, which the
//! table takes up as this process's holder (`net::adopt_exported`).

use toyos_abi::RawHandle;

/// A descriptor number.
pub type Fd = i32;

/// Descriptors and objects one table holds: as many as the kernel holds
/// handles for one process. The process's own handles take some of those, so
/// descriptors that are kernel handles meet the kernel's limit first, and
/// only descriptors that share an object or name none meet this one.
pub const MAX_FDS: usize = toyos_abi::RawHandle::MAX_SLOTS;

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
/// The tag of a file or directory a file server holds open (`crate::files`).
pub const SERVED_FILE: u32 = 1;
/// The tag of a stream's descriptor (`crate::net::stream`): a kernel handle,
/// let go by the copy of this crate that holds the stream's end, which only
/// that copy knows.
pub const STREAM: u32 = 2;
/// The tag of a directory the kernel serves, which has no handle
/// (`crate::files`).
pub const KERNEL_DIR: u32 = 3;
/// The tag of a listener's or a datagram socket's descriptor
/// (`crate::net::socket`): a copy of its bell, which a poller watches and no
/// file call reaches, let go by the copy of this crate that holds the
/// socket's end.
pub const SOCKET: u32 = 4;

/// What a descriptor names, as the table keeps it and as it crosses between
/// copies of this crate by value. Each one that names a handle has one owner:
/// an [`Object`] on its way in, then the table.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct Record {
    /// The kernel handle, or [`toyos_abi::HANDLE_INVALID`] for an opaque object.
    handle: RawHandle,
    /// Which kind of opaque object this crate keeps it is, [`UNTAGGED`] for a
    /// handle and for another library's.
    tag: u32,
    /// An opaque object's word, its owner's to interpret.
    word: u64,
    /// How an opaque object is let go once nothing names it.
    release: Option<extern "C" fn(u64)>,
}

impl Record {
    /// No object, for a free slot of the table.
    pub(crate) const FREE: Self = Self { handle: toyos_abi::HANDLE_INVALID, tag: UNTAGGED, word: 0, release: None };

    /// Let the object go: an opaque one to its library, a handle closed (a
    /// stream's end with its last handle, `net::stream::let_go`).
    ///
    /// # Safety
    /// This is the record's one owner, and nothing names it afterwards.
    unsafe fn release(self) {
        match self.release {
            Some(release) => release(self.word),
            // SAFETY: the caller's: the handle is given up here.
            None => crate::net::stream::let_go(unsafe { crate::OwnedHandle::from_raw(self.handle) }),
        }
    }
}

/// An object a descriptor is about to name, owned: [`install`] and [`replace`]
/// give it to the table, and one dropped instead is let go.
#[derive(Debug)]
pub struct Object(Record);

impl Object {
    /// A kernel handle the table owns from here on: it is closed when the
    /// object is released.
    pub fn handle(handle: crate::OwnedHandle) -> Self {
        Self(Record { handle: handle.into_raw(), tag: UNTAGGED, word: 0, release: None })
    }

    /// A stream's handle, which `release`, of the copy of this crate holding
    /// the stream's end, lets go of once no descriptor and no call names it.
    pub(crate) fn stream(handle: crate::OwnedHandle, release: extern "C" fn(u64)) -> Self {
        let handle = handle.into_raw();
        Self(Record { handle, tag: STREAM, word: handle.0 as u64, release: Some(release) })
    }

    /// A listener's or a datagram socket's bell, which `release`, of the copy
    /// of this crate holding the socket's end, lets go of as [`Self::stream`].
    pub(crate) fn socket(handle: crate::OwnedHandle, release: extern "C" fn(u64)) -> Self {
        let handle = handle.into_raw();
        Self(Record { handle, tag: SOCKET, word: handle.0 as u64, release: Some(release) })
    }

    /// An object a library keeps itself, `word`, which `release` is called
    /// with once no descriptor and no call names it.
    pub const fn opaque(word: u64, release: extern "C" fn(u64)) -> Self {
        Self::tagged(UNTAGGED, word, release)
    }

    /// An opaque object of a kind this crate keeps, which [`Lent::tag`] names.
    pub(crate) const fn tagged(tag: u32, word: u64, release: extern "C" fn(u64)) -> Self {
        Self(Record { handle: toyos_abi::HANDLE_INVALID, tag, word, release: Some(release) })
    }

    /// The record, now the receiver's to own.
    fn into_record(self) -> Record {
        let record = self.0;
        #[expect(clippy::disallowed_methods, reason = "the record's ownership moves to the receiver")]
        core::mem::forget(self);
        record
    }
}

impl Drop for Object {
    fn drop(&mut self) {
        // SAFETY: an object not given to the table is its record's one owner.
        unsafe { self.0.release() }
    }
}

/// A descriptor's object as a call holds it, for `'a`.
#[derive(Clone, Copy, Debug)]
pub struct Lent<'a> {
    record: Record,
    _hold: core::marker::PhantomData<&'a Hold>,
}

impl<'a> Lent<'a> {
    /// Which kind of opaque object this crate keeps it is, [`UNTAGGED`] for a
    /// handle and for another library's.
    pub fn tag(&self) -> u32 {
        self.record.tag
    }

    /// An opaque object's word.
    pub fn word(&self) -> u64 {
        self.record.word
    }

    /// The kernel handle the object holds, a stream's and a socket's bell
    /// included: `None` for an opaque object.
    pub fn handle(&self) -> Option<crate::BorrowedHandle<'a>> {
        // SAFETY: the table owns the handle and does not let it go while the
        // call holding the object lasts.
        (self.record.handle != toyos_abi::HANDLE_INVALID).then(|| unsafe { crate::BorrowedHandle::borrow_raw(self.record.handle) })
    }

    /// The kernel handle a kernel call on the descriptor is made on: `None`
    /// for an object a library keeps that is no handle.
    pub fn kernel_handle(&self) -> Option<crate::BorrowedHandle<'a>> {
        (self.record.release.is_none() || self.record.tag == STREAM).then(|| self.handle()).flatten()
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
    fn toyos_fd_install(record: Record, inheritable: bool, floor: Fd) -> i32;
    fn toyos_fd_acquire(fd: Fd, out: *mut Record) -> i32;
    fn toyos_fd_unuse(token: i32);
    fn toyos_fd_close(fd: Fd) -> i32;
    fn toyos_fd_dup(fd: Fd, floor: Fd, inheritable: bool) -> i32;
    fn toyos_fd_dup2(old: Fd, new: Fd, inheritable: bool) -> i32;
    fn toyos_fd_replace(fd: Fd, record: Record) -> i32;
    fn toyos_fd_inheritable(fd: Fd) -> i32;
    fn toyos_fd_set_inheritable(fd: Fd, inheritable: bool) -> i32;
}

/// Name `object` by the lowest free descriptor at or above `floor`. On
/// `Full` the object is let go, so nothing it held is left behind.
pub fn install(object: Object, inheritable: bool, floor: Fd) -> Result<Fd, Error> {
    // SAFETY: the owner's definition below, which takes the record's ownership.
    decode(unsafe { toyos_fd_install(object.into_record(), inheritable, floor) })
}

/// `fd`'s object, held until the [`Hold`] drops: `BadFd` for a descriptor
/// not open.
pub fn hold(fd: Fd) -> Result<Hold, Error> {
    let mut record = Record::FREE;
    // SAFETY: `record` is a live place for the answer.
    let token = decode(unsafe { toyos_fd_acquire(fd, &mut record) })?;
    Ok(Hold { token, record })
}

/// A call's hold of a descriptor's object: a `close` meanwhile releases the
/// object only once this drops.
#[must_use]
pub struct Hold {
    token: i32,
    record: Record,
}

impl Hold {
    pub fn object(&self) -> Lent<'_> {
        Lent { record: self.record, _hold: core::marker::PhantomData }
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        // SAFETY: a token `toyos_fd_acquire` answered, given back once.
        unsafe { toyos_fd_unuse(self.token) }
    }
}

/// `f` on `fd`'s object, held for the call: `BadFd` for a descriptor not
/// open.
pub fn with<R>(fd: Fd, f: impl FnOnce(Lent<'_>) -> R) -> Result<R, Error> {
    let held = hold(fd)?;
    Ok(f(held.object()))
}

/// Close `fd`, letting its object go once no call holds it.
///
/// # Safety
/// The caller owns `fd`: nothing else (an `OwnedFd`, a `File`, a stream)
/// answers for it or uses it afterwards. As POSIX's `close` would, a close of
/// another owner's descriptor ends the handle that owner still lends.
pub unsafe fn close(fd: Fd) -> Result<(), Error> {
    // SAFETY: plain values.
    decode(unsafe { toyos_fd_close(fd) }).map(drop)
}

pub fn dup(fd: Fd, floor: Fd, inheritable: bool) -> Result<Fd, Error> {
    // SAFETY: plain values.
    decode(unsafe { toyos_fd_dup(fd, floor, inheritable) })
}

/// POSIX's `dup2`: `new` on `old`'s object, after closing what `new` named;
/// onto itself, nothing but `old` checked open.
///
/// # Safety
/// As [`close`]'s, for `new`.
pub unsafe fn dup2(old: Fd, new: Fd, inheritable: bool) -> Result<Fd, Error> {
    // SAFETY: plain values.
    decode(unsafe { toyos_fd_dup2(old, new, inheritable) })
}

/// Make `fd` name `object` in place, letting go what it named: a socket
/// that connects becomes its stream at the same number. On `BadFd` or
/// `Full` the object is let go.
///
/// # Safety
/// As [`close`]'s, for `fd`.
pub unsafe fn replace(fd: Fd, object: Object) -> Result<(), Error> {
    // SAFETY: the owner's definition below, which takes the record's ownership.
    decode(unsafe { toyos_fd_replace(fd, object.into_record()) }).map(drop)
}

/// What a child is given for `fd`, and its kind: a copy of the kernel handle
/// it names, or for a stream, a listener or a datagram socket an end of a new
/// holder of it (`net::stream::export`, `net::socket::export`). `None` for a
/// descriptor that is not open, as a child of POSIX's holds no closed one.
/// Refused for an object that is no kernel handle, for one the kernel will
/// not duplicate, and for a socket netstack makes no holder of.
pub fn copy(fd: Fd) -> Result<Option<(crate::OwnedHandle, Kind)>, crate::spawn::Refused> {
    use crate::spawn::Refused;
    let copied = with(fd, |o| match (o.handle(), o.kernel_handle()) {
        (Some(bell), _) if o.tag() == SOCKET && crate::net::socket::is_socket(bell) => {
            crate::net::socket::export(bell).map(|end| (end, Kind::Exported)).map_err(Refused::Export)
        }
        (_, Some(handle)) if crate::net::stream::is_stream(handle) => {
            crate::net::stream::export(handle).map(|end| (end, Kind::Exported)).map_err(Refused::Export)
        }
        (_, Some(handle)) if o.tag() != STREAM => handle.try_clone_to_owned().map(|copy| (copy, Kind::Handle)).map_err(Refused::Syscall),
        // A socket another copy of this crate holds the end of, or no handle.
        _ => Err(Refused::Syscall(toyos_abi::syscall::SyscallError::NotSupported)),
    });
    copied.map_or(Ok(None), |copied| copied.map(Some))
}

/// What an `fd:` endowment's handle is, as its label's suffix names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The kernel handle the descriptor names: `fd:<n>`.
    Handle,
    /// A stream's, a listener's or a datagram socket's exported end, which
    /// the child takes up as its holder: `fd:<n>:exported`.
    Exported,
}

impl Kind {
    /// What a label carries past the number.
    pub const fn suffix(self) -> &'static str {
        match self {
            Self::Handle => "",
            Self::Exported => ":exported",
        }
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

/// The descriptor an `fd:<n>` or `fd:<n>:<kind>` endowment's label names, `n`
/// in canonical decimal, and its kind: `None` for any other label, and for a
/// number past the table.
pub fn endowed_fd(label: &str) -> Option<(Fd, Kind)> {
    let rest = label.strip_prefix(toyos_abi::syscall::FD_PREFIX)?;
    let (digits, kind) = match rest.split_once(':') {
        None => (rest, Kind::Handle),
        Some((digits, "exported")) => (digits, Kind::Exported),
        Some(_) => return None,
    };
    let canonical = digits.bytes().all(|b| b.is_ascii_digit()) && (digits == "0" || !digits.starts_with('0'));
    let n: Fd = digits.parse().ok().filter(|_| canonical)?;
    (0..MAX_FDS as Fd).contains(&n).then_some((n, kind))
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

    /// Install each `fd:` endowment at its number, a socket's taken up as
    /// this process's holder first. One whose number another already took,
    /// or whose socket this process holds no room for, is refused, its handle
    /// closed: how many were.
    fn endow(table: &mut Table<MAX_FDS>, endowed: &Endowments) -> Result<usize, Fault> {
        let mut refused = 0;
        for label in endowed.labels() {
            let Some((fd, kind)) = endowed_fd(label) else { continue };
            let Some(handle) = endowed.take::<OwnedHandle>(label) else { continue };
            let object = match kind {
                Kind::Handle => Object::handle(handle),
                Kind::Exported => match crate::net::adopt_exported(handle) {
                    Some(object) => object,
                    None => {
                        refused += 1;
                        continue;
                    }
                },
            };
            let record = object.into_record();
            match table.install_at(fd, record, true) {
                // The table owns the object from here on.
                Ok(()) => {}
                Err(Fault::Refused(_)) => {
                    // SAFETY: refused, so still this function's alone.
                    unsafe { record.release() };
                    refused += 1;
                }
                Err(Fault::Broken) => return Err(Fault::Broken),
            }
        }
        Ok(refused)
    }

    /// What the table answered it no longer names, let go.
    fn released(record: Option<Record>) {
        if let Some(record) = record {
            // SAFETY: the table answers a record only once it names it no more.
            unsafe { record.release() };
        }
    }

    #[no_mangle]
    extern "C" fn toyos_fd_install(record: Record, inheritable: bool, floor: Fd) -> i32 {
        table(|t| t.install(record, inheritable, floor)).unwrap_or_else(|e| {
            // SAFETY: refused, so the record is still the caller's, given here.
            unsafe { record.release() };
            encode(e)
        })
    }

    #[no_mangle]
    unsafe extern "C" fn toyos_fd_acquire(fd: Fd, out: *mut Record) -> i32 {
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
    extern "C" fn toyos_fd_replace(fd: Fd, record: Record) -> i32 {
        match table(|t| t.replace(fd, record)) {
            Ok(old) => {
                released(old);
                0
            }
            Err(e) => {
                // SAFETY: as in `toyos_fd_install`.
                unsafe { record.release() };
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
    fn an_fd_label_names_its_number_and_kind_and_no_other_label_names_one() {
        assert_eq!(endowed_fd("fd:0"), Some((0, Kind::Handle)));
        assert_eq!(endowed_fd("fd:4095"), Some((4095, Kind::Handle)));
        assert_eq!(endowed_fd("fd:3:exported"), Some((3, Kind::Exported)));
        for kind in [Kind::Handle, Kind::Exported] {
            assert_eq!(endowed_fd(&format!("fd:7{}", kind.suffix())), Some((7, kind)), "a label of its own");
        }
        for other in [
            "fd:4096", "fd:-1", "fd:", "fd:1x", "svc", "fd1", "dev:fd:1", "fd:+3", "fd:03", "fd:00", "fd: 3",
            "fd:3:", "fd:3:file", "fd:3:stream", "fd:3:exported:", "fd::exported", "fd:4096:exported",
        ] {
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
