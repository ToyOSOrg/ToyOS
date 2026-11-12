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

use alloc::string::String;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicU32, AtomicU8, Ordering};

use toyos_abi::handle::{ObjectKind, HANDLE_INVALID};
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

/// This process's endowment table, read once.
pub struct Endowments {
    /// What its parent calls each handle, in the order endowed.
    names: [&'static str; MAX_SPAWN_ENDOWMENTS],
    /// **Taken by swap**, so a name answers exactly once: the second `take`
    /// finds [`HANDLE_INVALID`] and returns `None` rather than a second owner
    /// of one handle.
    handles: [AtomicU32; MAX_SPAWN_ENDOWMENTS],
    count: usize,
}

impl Endowments {
    /// This process's own, read on first use.
    pub fn get() -> &'static Endowments {
        TABLE.get()
    }

    /// The table of a process whose parent called its `i`th endowment
    /// `names[i]`: the handle is `RawHandle::endowed(i)`.
    fn of(names: [&'static str; MAX_SPAWN_ENDOWMENTS], count: usize) -> Self {
        let handles = core::array::from_fn(|i| AtomicU32::new(if i < count { RawHandle::endowed(i).0 } else { HANDLE_INVALID.0 }));
        Self { names, handles, count }
    }

    /// Take the handle named `label`, as `T`. `None` once, for a name this
    /// process was not given; `None` again for one already taken; and `None`
    /// for a service's, which stays ([`connector`]).
    pub fn take<T: FromHandle>(&self, label: &str) -> Option<T> {
        if service_of(label).is_some() {
            return None;
        }
        let i = self.index_of(label)?;
        let raw = self.handles[i].swap(HANDLE_INVALID.0, Ordering::AcqRel);
        if raw == HANDLE_INVALID.0 {
            return None;
        }
        // The swap is what makes this the only caller to see the handle, and
        // the kernel put it in this process's table at spawn.
        let handle = OwnedHandle(RawHandle(raw));
        // SAFETY: what this process was endowed is its parent's to say, which
        // made it and can end it as it likes.
        Some(unsafe { T::from_handle(handle) })
    }

    /// Whether `label` names something this process still holds.
    pub fn holds(&self, label: &str) -> bool {
        self.index_of(label)
            .is_some_and(|i| self.handles[i].load(Ordering::Acquire) != HANDLE_INVALID.0)
    }

    /// Every name in the table, taken or not: for a diagnostic, and for a
    /// family of names such as `toyos::fd`'s, each then taken by name.
    pub fn labels(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.names[..self.count].iter().copied()
    }

    fn index_of(&self, label: &str) -> Option<usize> {
        self.names[..self.count].iter().position(|name| *name == label)
    }

    /// Every service's connector, by its whole label, in the table's order.
    fn services(&self) -> impl Iterator<Item = (&'static str, BorrowedHandle<'static>)> + '_ {
        self.labels().zip(&self.handles).filter(|(label, _)| service_of(label).is_some()).map(|(label, handle)| {
            // SAFETY: `take` gives no service's handle away, so it stays in
            // this process's table for its life.
            (label, unsafe { BorrowedHandle::borrow_raw(RawHandle(handle.load(Ordering::Relaxed))) })
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
        .or_else(|| {
            let (_, connector) = adopted().iter().find(|(held, _)| held == name)?;
            // SAFETY: an adopted connector is never dropped.
            Some(unsafe { BorrowedHandle::borrow_raw(connector.as_handle().raw()) })
        })
}

/// The name of every service this process holds, as [`connector`] takes it.
pub fn services() -> impl Iterator<Item = &'static str> {
    let endowed = Endowments::get().services().filter_map(|(label, _)| service_of(label));
    endowed.chain(adopted().iter().map(|(name, _)| name.as_str()))
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
/// or `None`. Borrowed: std connects through it for every launch, and a
/// parent that hands it on endows a duplicate.
pub fn launcher() -> Option<&'static Connector> {
    LAUNCHER.0.get_or_init(|| Endowments::get().take::<Connector>(crate::launch::LAUNCHER)).as_ref()
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
/// adopted any or has looked one up.
pub fn adopt(services: Vec<(String, Connector)>) -> Result<(), Vec<(String, Connector)>> {
    let mut offered = Some(services);
    ADOPTED.0.get_or_init(|| offered.take().expect("built once"));
    match offered {
        None => Ok(()),
        Some(services) => Err(services),
    }
}

fn adopted() -> &'static [(String, Connector)] {
    ADOPTED.0.get_or_init(Vec::new)
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
static ADOPTED: AdoptedCell = AdoptedCell(Once::new());

struct EndowTable(Once<Endowments>);
struct LauncherCell(Once<Option<Connector>>);
struct ProcessCell(Once<Process>);
struct AdoptedCell(Once<Vec<(String, Connector)>>);

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
