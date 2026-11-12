//! What this process was given, and the only place a name resolves.
//!
//! There is no global registry and no connect-by-name. A process learns what
//! it holds from its own endowment table: the handles its parent moved in at
//! spawn, and what its parent calls each, which the parent wrote into the
//! child's startup block ([`crate::startup`]). The kernel holds no name. A
//! name a process was not given resolves to nothing, and there is no second
//! place to ask.
//!
//! **A name is a local name in one process's own table**, so guessing one buys
//! nothing: the table is not enumerable across processes and a name not in
//! yours resolves to nothing wherever it came from.
//!
//! **A service is a connector under [`SVC_PREFIX`] or [`PROVIDE_PREFIX`]**,
//! which stays in the table for the process's life: [`service`] connects
//! through it, and a helper this process spawns is given a duplicate
//! (`crate::spawn`). Every other name is taken, once ([`Endowments::take`]),
//! and goes to a child only by an endowment its spawn names.
//!
//! **A process holds what it did with its endowments once**, however many
//! copies of this crate it links: which names were taken, its launcher and the
//! services it adopted. The copy that owns the descriptor table (`crate::fd`)
//! keeps that state and defines `toyos_endow_*`, which every copy reaches it
//! through, as it does the stdio state (`crate::log::stdio`).

use alloc::string::String;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicU8, Ordering};
#[cfg(any(test, feature = "fd-table", not(target_os = "toyos")))]
use core::sync::atomic::AtomicBool;

use toyos_abi::handle::ObjectKind;
#[cfg(any(test, feature = "fd-table", not(target_os = "toyos")))]
use toyos_abi::handle::HANDLE_INVALID;
use toyos_abi::syscall::{DeviceRequest, DeviceType, SyscallError, MAX_SPAWN_ENDOWMENTS};

use crate::ipc::Connection;
use crate::job::Job;
use crate::port::{Acceptor, Connector};
use crate::process::Process;
use crate::syscap::SysCap;
use crate::{AsHandle, BorrowedHandle, Device, OwnedHandle, RawHandle};

/// The name of the system capability a program's row grants it.
pub const SYSCAP_LABEL: &str = "syscap";
/// `svc:<name>`: a connector to a service this program's row gives it, a
/// directory capability's `fs:<directory>` among them.
pub const SVC_PREFIX: &str = "svc:";
/// `provide:<name>`: a connector to a service whoever launched this program
/// gave it beside its row, which receives the name, and which it passes on to
/// what it launches ([`provided`]).
pub const PROVIDE_PREFIX: &str = "provide:";
/// `serve:<name>`: the acceptor of a machine-wide port this program serves.
pub const SERVE_PREFIX: &str = "serve:";
/// `dev:<class>`: the claim for a device class this program was given.
pub const DEV_PREFIX: &str = "dev:";
/// `fd:<n>`: the child's descriptor `n`, which its descriptor table installs
/// at that number (`crate::fd`).
pub const FD_PREFIX: &str = "fd:";

/// The prefixes of a service's name, in the order [`service`] looks one up.
const SERVICE_PREFIXES: [&str; 2] = [SVC_PREFIX, PROVIDE_PREFIX];

/// `name` under `prefix`: the label a spawn endows a service's connector by.
pub fn service_label(prefix: &str, name: &str) -> String {
    alloc::format!("{prefix}{name}")
}

/// The service a label names, if it is a service's.
pub(crate) fn service_of(label: &str) -> Option<&str> {
    SERVICE_PREFIXES.iter().find_map(|prefix| label.strip_prefix(prefix))
}

/// Why a service name did not become a connection.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EndowError {
    /// This process was not given the service, or was given its connector
    /// without the right to connect: a fact about this process, not about the
    /// machine.
    /// There is no "not yet": every port exists before either end's process
    /// runs.
    NotEndowed,
    /// The server exited. Its acceptor's last handle went with it and the port
    /// is closed for good.
    ServerGone,
    /// The kernel refused for a reason of its own — a full queue, a full table.
    Refused(SyscallError),
}

/// A typed wrapper over a handle it owns.
pub trait FromHandle {
    /// # Safety
    /// Every call of this type is one the object `handle` names can take:
    /// the kernel ends a process that makes one on any other. For an
    /// [`OfKind`] type, that is an object of one of its kinds. For a claim's
    /// class type ([`ClaimType`]), it is a claim on a device of that class.
    /// Of a handle a peer sent, only the kernel says which kind it is:
    /// [`ReceivedHandle::typed`](crate::ipc::ReceivedHandle::typed).
    unsafe fn from_handle(handle: OwnedHandle) -> Self;
}

/// A typed wrapper whose every call any object of one of [`Self::KINDS`]
/// takes, so the kernel's word on a received handle's kind is enough to make
/// one ([`ReceivedHandle::typed`](crate::ipc::ReceivedHandle::typed)). A
/// claim's class types are not: a claim's kind does not say its class.
pub trait OfKind: FromHandle {
    const KINDS: &'static [ObjectKind];
}

/// A type the system capability mints a device claim as: [`Device`], or a
/// class's type over it. The SDK does not pair the type with the class a call
/// claims: a caller that names a class type keeps it to the class it asks for.
pub trait ClaimType: FromHandle {}

/// Whether `kinds` names `kind`.
pub(crate) const fn names(kinds: &[ObjectKind], kind: ObjectKind) -> bool {
    let mut i = 0;
    while i < kinds.len() {
        if kinds[i] as u32 == kind as u32 {
            return true;
        }
        i += 1;
    }
    false
}

/// [`FromHandle`] and [`OfKind`] for each type, made by its constructor.
macro_rules! from_handle {
    ($($ty:ty: $($kind:ident)|+ => $make:expr),+ $(,)?) => {
        $(impl $crate::endow::FromHandle for $ty {
            unsafe fn from_handle(handle: $crate::OwnedHandle) -> Self {
                let make: fn($crate::OwnedHandle) -> Self = $make;
                make(handle)
            }
        }

        impl $crate::endow::OfKind for $ty {
            const KINDS: &'static [toyos_abi::handle::ObjectKind] = &[$(toyos_abi::handle::ObjectKind::$kind),+];
        })+
    };
}
pub(crate) use from_handle;

from_handle! {
    Acceptor: Acceptor => Acceptor,
    Connector: Connector => Connector,
    SysCap: SysCap => SysCap,
    Job: Job => Job,
    Process: Process => Process,
    Device: Device => Device,
    crate::Pipe: PipeRead | PipeWrite => crate::Pipe,
    crate::ipc::Connection: Connection => crate::ipc::Connection,
}

/// Any handle, untyped.
impl FromHandle for OwnedHandle {
    unsafe fn from_handle(handle: OwnedHandle) -> Self {
        handle
    }
}

impl OfKind for OwnedHandle {
    const KINDS: &'static [ObjectKind] = &ObjectKind::ALL;
}

impl ClaimType for Device {}

/// The class types over a claim: [`FromHandle`] and [`ClaimType`], never
/// [`OfKind`].
macro_rules! class_of_claim {
    ($($ty:ty),+ $(,)?) => {
        $(impl FromHandle for $ty {
            unsafe fn from_handle(handle: OwnedHandle) -> Self {
                Self(Device(handle))
            }
        }

        impl ClaimType for $ty {})+
    };
}

class_of_claim!(crate::FramebufferDev, crate::PciDev, crate::AcpiDev, crate::UartDev);

/// This process's endowment table, read once by each copy of this crate:
/// what its parent calls each handle. What was taken from it is the
/// process's, held once ([`State`]).
pub struct Endowments {
    /// What its parent calls each handle, in the order endowed.
    names: [&'static str; MAX_SPAWN_ENDOWMENTS],
    count: usize,
    /// A test's table, which stands for a process, carries the state the
    /// owner holds.
    #[cfg(test)]
    state: &'static State,
}

impl Endowments {
    /// This process's own, read on first use.
    pub fn get() -> &'static Endowments {
        TABLE.get()
    }

    /// The table of a process whose parent called its `i`th endowment
    /// `names[i]`: the handle is `RawHandle::endowed(i)`.
    fn of(names: [&'static str; MAX_SPAWN_ENDOWMENTS], count: usize) -> Self {
        Self {
            names,
            count,
            #[cfg(test)]
            state: alloc::boxed::Box::leak(alloc::boxed::Box::new(State::new())),
        }
    }

    /// Take the handle named `label`, as `T`. `None` once, for a name this
    /// process was not given; `None` again for one already taken, by any copy
    /// of this crate; and `None` for a service's, which stays ([`connector`]).
    pub fn take<T: FromHandle>(&self, label: &str) -> Option<T> {
        let raw = held::take(self, label)?;
        // SAFETY: what this process was endowed is its parent's to say, which
        // made it and can end it as it likes; and the process's one record of
        // what was taken gave the handle to this caller alone.
        Some(unsafe { T::from_handle(OwnedHandle(raw)) })
    }

    /// Whether `label` names something this process still holds.
    pub fn holds(&self, label: &str) -> bool {
        held::holds(self, label)
    }

    /// Every name in the table, taken or not: for a diagnostic, and for a
    /// family of names such as `toyos::fd`'s, each then taken by name.
    pub fn labels(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.names[..self.count].iter().copied()
    }

    #[cfg(any(test, feature = "fd-table", not(target_os = "toyos")))]
    fn index_of(&self, label: &str) -> Option<usize> {
        self.names[..self.count].iter().position(|name| *name == label)
    }

    /// Every service's connector, by its whole label, in the table's order.
    fn services(&self) -> impl Iterator<Item = (&'static str, BorrowedHandle<'static>)> + '_ {
        self.labels().enumerate().filter(|(_, label)| service_of(label).is_some()).map(|(i, label)| {
            // SAFETY: no take gives a service's handle away, so it stays in
            // this process's table for its life.
            (label, unsafe { BorrowedHandle::borrow_raw(RawHandle::endowed(i)) })
        })
    }
}

/// The connector to the service `name` this process holds, the one its row
/// gave before one it was provided, then one it [`adopt`]ed: borrowed for the
/// process's life. A holder that wants its own duplicates it.
pub fn connector(name: &str) -> Option<BorrowedHandle<'static>> {
    let table = Endowments::get();
    SERVICE_PREFIXES
        .iter()
        .find_map(|prefix| table.services().find(|(label, _)| label.strip_prefix(prefix) == Some(name)))
        .map(|(_, connector)| connector)
        .or_else(|| held::adopted().iter().find(|service| service.name() == name).map(Adopted::connector))
}

/// The name of every service this process holds, as [`connector`] takes it.
pub fn services() -> impl Iterator<Item = &'static str> {
    let endowed = Endowments::get().services().filter_map(|(label, _)| service_of(label));
    endowed.chain(held::adopted().iter().map(Adopted::name))
}

/// The label and connector of every service a helper this process spawns is
/// given a duplicate of: what it was endowed, never what it adopted.
pub(crate) fn inheritable() -> impl Iterator<Item = (&'static str, BorrowedHandle<'static>)> {
    Endowments::get().services()
}

/// Every connector whoever launched this process provided, by name: what it
/// passes on to a program it launches in turn, whose row decides everything
/// else that program gets. Borrowed, so the caller duplicates one per child.
pub fn provided() -> impl Iterator<Item = (&'static str, BorrowedHandle<'static>)> {
    Endowments::get().services().filter_map(|(label, connector)| Some((label.strip_prefix(PROVIDE_PREFIX)?, connector)))
}

/// The launcher this process was endowed under [`crate::launch::LAUNCHER`],
/// or `None`. Borrowed: the process keeps it once for every copy of this crate
/// ([`State`]), std connects through it for every launch, and a parent that
/// hands it on endows a duplicate.
pub fn launcher() -> Option<&'static Connector> {
    // This copy's wrapper over the process's one launcher, never dropped, so
    // nothing closes it.
    LAUNCHER.0.get_or_init(|| held::launcher(Endowments::get()).map(|raw| Connector(OwnedHandle(raw)))).as_ref()
}

/// This process's handle to itself, which it hands on for a child to be
/// placed under it: `WRITE`, `DUP` and `TRANSFER`, from the kernel, at
/// [`RawHandle::SELF`]. Borrowed: every launch duplicates it.
pub fn this_process() -> &'static Process {
    // The kernel starts every process holding it there, and nothing closes
    // it: the wrapper is never dropped.
    THIS_PROCESS.0.get_or_init(|| Process(OwnedHandle(RawHandle::SELF)))
}

/// Make `services` this process's own beside what it was endowed, for the
/// one process no parent endows any: the supervisor, which makes the
/// machine's ports itself and resolves its own files through them as every
/// program does. No child inherits them. Refused once this process has
/// adopted any or has looked one up, through any copy of this crate: the
/// process holds one set ([`State`]).
pub fn adopt(services: Vec<(String, Connector)>) -> Result<(), Vec<(String, Connector)>> {
    let offered: Vec<Adopted> = services
        .iter()
        .map(|(name, connector)| Adopted { name: name.as_ptr(), len: name.len(), handle: connector.as_handle().raw().0 })
        .collect();
    // SAFETY: each entry names a name of `services` and its connector, both
    // live for the call; on `true` the connectors are the process's set's,
    // and are given up here.
    if unsafe { held::adopt(&offered) } {
        for (_, connector) in services {
            let _ = OwnedHandle::from(connector).into_raw();
        }
        Ok(())
    } else {
        Err(services)
    }
}

/// Open a connection to the service `name`.
///
/// The one place a name becomes a connection. It works from the caller's first
/// instruction whether or not the server has reached `accept` or has even been
/// spawned, because the port existed before either process did.
pub fn service(name: &str) -> Result<Connection, EndowError> {
    let connector = connector(name).ok_or(EndowError::NotEndowed)?;
    crate::port::connect(connector).map_err(|e| match e {
        SyscallError::PermissionDenied => EndowError::NotEndowed,
        SyscallError::Gone => EndowError::ServerGone,
        other => EndowError::Refused(other),
    })
}

/// The acceptor of a name the manifest says this program serves.
///
/// A refusal is a build-system bug rather than a race: `build/build.rs` checked
/// the manifest before the image was written, so there is no other process to
/// have taken the name and no name to take.
pub fn acceptor(name: &str) -> Option<Acceptor> {
    with_prefixed(SERVE_PREFIX, name, |label| Endowments::get().take::<Acceptor>(label))
}

/// The claim for a device class the manifest says this program gets.
///
/// `None` is a machine that had no such device when the supervisor asked, or a program
/// the manifest gives none — the honest answer, and the one soundserver degrades
/// on.
pub fn device<T: FromHandle>(class: DeviceType) -> Option<T> {
    with_prefixed(DEV_PREFIX, class.class_name(), |label| Endowments::get().take::<T>(label))
}

/// The claim the manifest's entry `name` says this program gets, under the
/// label the supervisor endows it by: the entry's own spelling. A PCI
/// function's is `toyos-manifest`'s matcher, which a driver spells with the
/// same type the manifest is parsed with.
pub fn claim<T: FromHandle>(name: &str) -> Option<T> {
    with_prefixed(DEV_PREFIX, name, |label| Endowments::get().take::<T>(label))
}

/// The claim for one legacy ISA function the manifest says this program gets,
/// looked up by the entry's own spelling.
pub fn isa<T: FromHandle>(set: toyos_abi::syscall::IsaId) -> Option<T> {
    let mut buf = [0u8; DeviceRequest::MAX_NAME];
    claim(DeviceRequest::Isa(set).write_name(&mut buf)?)
}

/// Compose `<prefix><name>` on the stack: a name too long for it matches
/// nothing rather than being truncated into a label that happens to exist.
fn with_prefixed<T>(prefix: &str, name: &str, f: impl FnOnce(&str) -> Option<T>) -> Option<T> {
    let mut buf = [0u8; 128];
    let total = prefix.len() + name.len();
    if total > buf.len() {
        return None;
    }
    buf[..prefix.len()].copy_from_slice(prefix.as_bytes());
    buf[prefix.len()..total].copy_from_slice(name.as_bytes());
    f(core::str::from_utf8(&buf[..total]).ok()?)
}

/// One value built exactly once, with no allocator and no `Drop`.
///
/// Whatever is in here lives for the process's life, which is what makes
/// handing out a `&'static` to it sound: nothing in the table is ever released
/// while the table exists.
pub(crate) struct Once<T> {
    state: AtomicU8,
    value: UnsafeCell<MaybeUninit<T>>,
}

const UNSET: u8 = 0;
const BUILDING: u8 = 1;
const READY: u8 = 2;

/// The cell is written once, under the `UNSET -> BUILDING` exchange, and read
/// only after `READY`.
unsafe impl<T: Send> Sync for Once<T> {}

impl<T> Once<T> {
    pub(crate) const fn new() -> Self {
        Self { state: AtomicU8::new(UNSET), value: UnsafeCell::new(MaybeUninit::uninit()) }
    }

    pub(crate) fn get_or_init(&self, build: impl FnOnce() -> T) -> &T {
        if self
            .state
            .compare_exchange(UNSET, BUILDING, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            // SAFETY: the exchange makes this thread the only writer, and no
            // reader proceeds before `READY` is stored below.
            unsafe { (*self.value.get()).write(build()) };
            self.state.store(READY, Ordering::Release);
        }
        while self.state.load(Ordering::Acquire) != READY {
            core::hint::spin_loop();
        }
        // SAFETY: `READY` is stored after the write, with release/acquire.
        unsafe { (*self.value.get()).assume_init_ref() }
    }
}

static TABLE: EndowTable = EndowTable(Once::new());
static LAUNCHER: LauncherCell = LauncherCell(Once::new());
static THIS_PROCESS: ProcessCell = ProcessCell(Once::new());

struct EndowTable(Once<Endowments>);
struct LauncherCell(Once<Option<Connector>>);
struct ProcessCell(Once<Process>);

impl EndowTable {
    /// The table its parent wrote, or the process's end: every channel a
    /// process speaks on is named by the table, so a malformed one is said on
    /// the `fd:2` it still names, if any, and by the exit status. Nothing
    /// here panics, since a panic's own words go through this table.
    fn get(&'static self) -> &'static Endowments {
        self.0.get_or_init(|| {
            let mut names = [""; MAX_SPAWN_ENDOWMENTS];
            match crate::startup::endowed(&mut names) {
                Ok(count) => Endowments::of(names, count),
                Err(why) => {
                    if let Some(stderr) = crate::startup::early(b"fd:2") {
                        let _ = toyos_abi::syscall::write_nonblock(RawHandle::endowed(stderr), why.line());
                    }
                    toyos_abi::syscall::exit(crate::startup::MALFORMED_STATUS)
                }
            }
        })
    }
}

/// What a process did with its endowments, held once for every copy of this
/// crate it links: which names were taken, its launcher and the services it
/// adopted.
#[cfg(any(test, feature = "fd-table", not(target_os = "toyos")))]
pub(crate) struct State {
    /// **Taken by swap**, so a name answers exactly once in the process: the
    /// second take, from whichever copy, finds it taken rather than making a
    /// second owner of one handle.
    taken: [AtomicBool; MAX_SPAWN_ENDOWMENTS],
    /// The launcher's number, taken once and never closed, or
    /// [`HANDLE_INVALID`] for none.
    launcher: Once<u32>,
    adopted: Once<AdoptedSet>,
}

#[cfg(any(test, feature = "fd-table", not(target_os = "toyos")))]
impl State {
    pub(crate) const fn new() -> Self {
        Self { taken: [const { AtomicBool::new(false) }; MAX_SPAWN_ENDOWMENTS], launcher: Once::new(), adopted: Once::new() }
    }

    /// The handle `table` names `label`, to this caller alone: `None` for a
    /// name it does not hold, one already taken, or a service's, which stays.
    fn take(&self, table: &Endowments, label: &str) -> Option<RawHandle> {
        if service_of(label).is_some() {
            return None;
        }
        let i = table.index_of(label)?;
        (!self.taken[i].swap(true, Ordering::AcqRel)).then_some(RawHandle::endowed(i))
    }

    fn holds(&self, table: &Endowments, label: &str) -> bool {
        table.index_of(label).is_some_and(|i| !self.taken[i].load(Ordering::Acquire))
    }

    /// The launcher, taken on the first ask and kept for the process's life.
    fn launcher(&self, table: &Endowments) -> Option<RawHandle> {
        let raw = *self.launcher.get_or_init(|| self.take(table, crate::launch::LAUNCHER).map_or(HANDLE_INVALID.0, |h| h.0));
        (raw != HANDLE_INVALID.0).then_some(RawHandle(raw))
    }

    /// Make `offered` the process's adopted set, unless a set was made or
    /// looked up before: whether it did.
    ///
    /// # Safety
    /// Each entry names `len` bytes of UTF-8 and a connector, both live for the
    /// call; on `true`, the connectors are the set's.
    unsafe fn adopt(&self, offered: &[Adopted]) -> bool {
        let mut made = false;
        self.adopted.get_or_init(|| {
            made = true;
            let kept: Vec<(String, OwnedHandle)> = offered
                .iter()
                // SAFETY: the caller's.
                .map(|entry| (String::from(unsafe { entry.text() }), OwnedHandle(RawHandle(entry.handle))))
                .collect();
            let entries = kept
                .iter()
                .map(|(name, connector)| Adopted { name: name.as_ptr(), len: name.len(), handle: connector.as_handle().raw().0 })
                .collect();
            AdoptedSet { _kept: kept, entries }
        });
        made
    }

    /// The adopted set, which this ends: no adoption follows a lookup.
    fn adopted(&'static self) -> &'static [Adopted] {
        &self.adopted.get_or_init(|| AdoptedSet { _kept: Vec::new(), entries: Vec::new() }).entries
    }
}

/// What a process adopted, kept for its life: the names and connectors, and
/// the entries every copy reads them through.
#[cfg(any(test, feature = "fd-table", not(target_os = "toyos")))]
struct AdoptedSet {
    /// What `entries` point into: never changed, and never dropped.
    _kept: Vec<(String, OwnedHandle)>,
    entries: Vec<Adopted>,
}

// SAFETY: `entries` points into `_kept`'s names, which are never changed or
// freed while the set exists, and the set is reached only by shared reference.
#[cfg(any(test, feature = "fd-table", not(target_os = "toyos")))]
unsafe impl Send for AdoptedSet {}

/// One adopted service as it crosses between copies of this crate: its
/// name's bytes and its connector's number.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct Adopted {
    name: *const u8,
    len: usize,
    handle: u32,
}

impl Adopted {
    /// # Safety
    /// `name` is `len` bytes of UTF-8, live for `'a`.
    unsafe fn text<'a>(&self) -> &'a str {
        // SAFETY: the caller's.
        unsafe { core::str::from_utf8_unchecked(core::slice::from_raw_parts(self.name, self.len)) }
    }

    /// The name of an entry of the process's set, which keeps it for its life.
    fn name(&'static self) -> &'static str {
        // SAFETY: an entry the set answered names its own kept name.
        unsafe { self.text() }
    }

    /// The connector of an entry of the process's set, which never closes it.
    fn connector(&'static self) -> BorrowedHandle<'static> {
        // SAFETY: the set holds the connector for the process's life.
        unsafe { BorrowedHandle::borrow_raw(RawHandle(self.handle)) }
    }
}

/// The process's [`State`] as each copy reaches it on ToyOS: through
/// `toyos_endow_*`, which the copy that owns the descriptor table defines
/// ([`owner`]).
#[cfg(all(target_os = "toyos", not(test)))]
mod held {
    use super::{Adopted, Endowments};
    use crate::RawHandle;
    use toyos_abi::handle::HANDLE_INVALID;

    // What every copy calls and the owner defines.
    extern "C" {
        /// The handle the process's table names `len` bytes of UTF-8 at
        /// `label`, to this caller alone, or [`HANDLE_INVALID`].
        fn toyos_endow_take(label: *const u8, len: usize) -> u32;
        fn toyos_endow_holds(label: *const u8, len: usize) -> bool;
        /// The launcher's number, or [`HANDLE_INVALID`].
        fn toyos_endow_launcher() -> u32;
        /// `count` entries, each a name and a connector the owner keeps on `true`.
        fn toyos_endow_adopt(offered: *const Adopted, count: usize) -> bool;
        /// The adopted set's entries, kept for the process's life, and how many.
        fn toyos_endow_adopted(count: *mut usize) -> *const Adopted;
    }

    fn handle(raw: u32) -> Option<RawHandle> {
        (raw != HANDLE_INVALID.0).then_some(RawHandle(raw))
    }

    pub(super) fn take(_: &Endowments, label: &str) -> Option<RawHandle> {
        // SAFETY: the owner's definition, which reads the label's bytes for
        // the call.
        handle(unsafe { toyos_endow_take(label.as_ptr(), label.len()) })
    }

    pub(super) fn holds(_: &Endowments, label: &str) -> bool {
        // SAFETY: as `take`'s.
        unsafe { toyos_endow_holds(label.as_ptr(), label.len()) }
    }

    pub(super) fn launcher(_: &Endowments) -> Option<RawHandle> {
        // SAFETY: the owner's definition, of a plain value.
        handle(unsafe { toyos_endow_launcher() })
    }

    /// # Safety
    /// As [`super::State::adopt`]'s.
    pub(super) unsafe fn adopt(offered: &[Adopted]) -> bool {
        // SAFETY: the caller's.
        unsafe { toyos_endow_adopt(offered.as_ptr(), offered.len()) }
    }

    pub(super) fn adopted() -> &'static [Adopted] {
        let mut count = 0;
        // SAFETY: the owner writes the count and answers its own entries,
        // which it keeps for the process's life.
        let entries = unsafe { toyos_endow_adopted(&mut count) };
        if count == 0 {
            return &[];
        }
        // SAFETY: as above, `count` entries at `entries`.
        unsafe { core::slice::from_raw_parts(entries, count) }
    }
}

/// Elsewhere, a state this copy holds itself: the one a test's table carries,
/// or, in a host build, which reads no endowment table, one that only links.
#[cfg(not(all(target_os = "toyos", not(test))))]
mod held {
    use super::{Adopted, Endowments, State};
    use crate::RawHandle;

    static STATE: State = State::new();

    fn state(_table: &Endowments) -> &'static State {
        #[cfg(test)]
        let state = _table.state;
        #[cfg(not(test))]
        let state = &STATE;
        state
    }

    pub(super) fn take(table: &Endowments, label: &str) -> Option<RawHandle> {
        state(table).take(table, label)
    }

    pub(super) fn holds(table: &Endowments, label: &str) -> bool {
        state(table).holds(table, label)
    }

    pub(super) fn launcher(table: &Endowments) -> Option<RawHandle> {
        state(table).launcher(table)
    }

    /// # Safety
    /// As [`super::State::adopt`]'s.
    pub(super) unsafe fn adopt(offered: &[Adopted]) -> bool {
        // SAFETY: the caller's.
        unsafe { STATE.adopt(offered) }
    }

    pub(super) fn adopted() -> &'static [Adopted] {
        STATE.adopted()
    }
}

/// The process's [`State`], in the copy that owns the descriptor table, and
/// the `toyos_endow_*` every copy reaches it through.
#[cfg(all(feature = "fd-table", not(test)))]
mod owner {
    use super::{Adopted, Endowments, State};
    use toyos_abi::handle::HANDLE_INVALID;

    static STATE: State = State::new();

    /// # Safety
    /// `label` is `len` bytes of UTF-8, live for `'a`.
    unsafe fn label<'a>(label: *const u8, len: usize) -> &'a str {
        // SAFETY: the caller's.
        unsafe { core::str::from_utf8_unchecked(core::slice::from_raw_parts(label, len)) }
    }

    #[no_mangle]
    unsafe extern "C" fn toyos_endow_take(label: *const u8, len: usize) -> u32 {
        // SAFETY: the caller's label, a `&str` of its own.
        STATE.take(Endowments::get(), unsafe { self::label(label, len) }).map_or(HANDLE_INVALID.0, |h| h.0)
    }

    #[no_mangle]
    unsafe extern "C" fn toyos_endow_holds(label: *const u8, len: usize) -> bool {
        // SAFETY: as in `toyos_endow_take`.
        STATE.holds(Endowments::get(), unsafe { self::label(label, len) })
    }

    #[no_mangle]
    extern "C" fn toyos_endow_launcher() -> u32 {
        STATE.launcher(Endowments::get()).map_or(HANDLE_INVALID.0, |h| h.0)
    }

    #[no_mangle]
    unsafe extern "C" fn toyos_endow_adopt(offered: *const Adopted, count: usize) -> bool {
        // SAFETY: the caller's `count` entries, live for the call, each as
        // `State::adopt` asks.
        unsafe { STATE.adopt(core::slice::from_raw_parts(offered, count)) }
    }

    #[no_mangle]
    unsafe extern "C" fn toyos_endow_adopted(count: *mut usize) -> *const Adopted {
        let entries = STATE.adopted();
        // SAFETY: the caller's word, live for the call.
        unsafe { count.write(entries.len()) };
        entries.as_ptr()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(names: &[&'static str]) -> Endowments {
        let mut all = [""; MAX_SPAWN_ENDOWMENTS];
        all[..names.len()].copy_from_slice(names);
        Endowments::of(all, names.len())
    }

    /// A taken handle's number, the wrapper given up so nothing closes it.
    fn taken(table: &Endowments, label: &str) -> Option<RawHandle> {
        table.take::<OwnedHandle>(label).map(OwnedHandle::into_raw)
    }

    /// The `i`th name is the `i`th endowed handle's, which answers once.
    #[test]
    fn a_name_is_the_handle_endowed_at_its_place_and_is_taken_once() {
        let table = table(&["fd:0", "syscap", "serve:net"]);
        assert!(table.holds("serve:net"));
        assert_eq!(taken(&table, "serve:net"), Some(RawHandle::endowed(2)));
        assert_eq!(taken(&table, "serve:net"), None);
        assert!(!table.holds("serve:net"));
        assert_eq!(taken(&table, "fd:0"), Some(RawHandle::endowed(0)));
        assert_eq!(taken(&table, "fd:1"), None);
        assert_eq!(taken(&table, ""), None, "a place past the table's count names nothing");
        assert_eq!(table.labels().collect::<Vec<_>>(), ["fd:0", "syscap", "serve:net"]);
    }

    /// A service's connector stays in the table: it is listed with its
    /// handle, and no `take` gives it away.
    #[test]
    fn a_service_stays_in_the_table() {
        let table = table(&["svc:net", "launcher", "provide:surface", "svc:fs:/tmp"]);
        assert_eq!(taken(&table, "svc:net"), None);
        assert_eq!(taken(&table, "provide:surface"), None);
        let held: Vec<_> = table.services().map(|(label, handle)| (label, handle.raw())).collect();
        assert_eq!(
            held,
            [("svc:net", RawHandle::endowed(0)), ("provide:surface", RawHandle::endowed(2)), ("svc:fs:/tmp", RawHandle::endowed(3))]
        );
        assert_eq!(taken(&table, "launcher"), Some(RawHandle::endowed(1)));
    }

    /// What a second copy of this crate in the process reads: the same table,
    /// and the process's one state.
    fn another_copy(table: &Endowments) -> Endowments {
        Endowments { names: table.names, count: table.count, state: table.state }
    }

    /// A name answers once in the process, whichever copy asks: a handle one
    /// copy took is refused to the other, which would otherwise be its second
    /// owner.
    #[test]
    fn a_name_answers_once_whichever_copy_takes_it() {
        let first = table(&["fd:0", "launcher", "syscap"]);
        let second = another_copy(&first);
        assert_eq!(taken(&second, "syscap"), Some(RawHandle::endowed(2)));
        assert_eq!(taken(&first, "syscap"), None, "a name the other copy took");
        assert!(!first.holds("syscap") && !second.holds("syscap"));
        assert!(first.holds("fd:0") && second.holds("fd:0"));
    }

    /// The launcher is taken once and lent to every copy: none takes it
    /// again, and none is told the process holds none.
    #[test]
    fn every_copy_is_lent_the_one_launcher() {
        let first = table(&["fd:0", "launcher"]);
        let second = another_copy(&first);
        assert_eq!(held::launcher(&first), Some(RawHandle::endowed(1)));
        assert_eq!(taken(&second, "launcher"), None, "the launcher the process keeps");
        assert_eq!(held::launcher(&second), Some(RawHandle::endowed(1)), "the launcher another copy asked for first");
        assert_eq!(held::launcher(&table(&["fd:0"])), None);
    }

    /// One adoption makes the set, and a lookup ends it: a second adoption is
    /// refused, its offer left with its caller.
    #[test]
    fn the_adopted_set_is_made_once_and_ended_by_a_lookup() {
        let state: &'static State = alloc::boxed::Box::leak(alloc::boxed::Box::new(State::new()));
        let name = "fs:/system";
        let offered = [Adopted { name: name.as_ptr(), len: name.len(), handle: 7 }];
        // SAFETY: a live name; the handle is a number the leaked set never closes.
        assert!(unsafe { state.adopt(&offered) });
        assert!(!unsafe { state.adopt(&offered) }, "a second adoption");
        let set: Vec<_> = state.adopted().iter().map(|entry| (entry.name(), entry.connector().raw())).collect();
        assert_eq!(set, [("fs:/system", RawHandle(7))]);

        let looked_up: &'static State = alloc::boxed::Box::leak(alloc::boxed::Box::new(State::new()));
        assert!(looked_up.adopted().is_empty());
        // SAFETY: as above.
        assert!(!unsafe { looked_up.adopt(&offered) }, "an adoption after a lookup");
    }

    #[test]
    fn a_service_is_named_under_either_prefix_and_nothing_else_is_one() {
        assert_eq!(service_of("svc:fs:/home/toy"), Some("fs:/home/toy"));
        assert_eq!(service_of("provide:surface"), Some("surface"));
        assert_eq!(service_of(&service_label(SVC_PREFIX, "net")), Some("net"));
        for label in ["serve:net", "syscap", "fd:2", "launcher", "svc", "provide"] {
            assert_eq!(service_of(label), None, "{label}");
        }
    }
}
