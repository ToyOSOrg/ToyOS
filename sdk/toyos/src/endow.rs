//! What this process was given, and the only place a service name resolves.
//!
//! There is no global registry and no connect-by-name. A process learns what
//! it holds from its own endowment table — `(label, handle)` pairs its parent
//! moved in at spawn — and resolves a service through the namespace in it. A
//! name it was not given resolves to nothing, and there is no second place to
//! ask.
//!
//! **A label is a local name in one process's own table**, so guessing one buys
//! nothing: the table is not enumerable across processes and a name not in
//! yours resolves to nothing wherever it came from.

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicU32, AtomicU8, Ordering};

use toyos_abi::handle::HANDLE_INVALID;
use toyos_abi::syscall::{
    self, DeviceRequest, DeviceType, SyscallError, MAX_ENDOWMENTS, MAX_LABELS_LEN,
};

use crate::ipc::Connection;
use crate::job::Job;
use crate::namespace::Namespace;
use crate::port::{Acceptor, Connector};
use crate::process::Process;
use crate::syscap::SysCap;
use crate::{Device, OwnedHandle, RawHandle};

pub use toyos_abi::syscall::{DEV_PREFIX, PROVIDE_PREFIX, SELF_LABEL, SERVE_PREFIX, SVC_LABEL, SYSCAP_LABEL};

/// Why a service name did not become a connection.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EndowError {
    /// The name is not in the namespace this process was given — a fact about
    /// this process, not about the machine. There is no "not yet": every port
    /// exists before either end's process runs.
    NotEndowed,
    /// The server exited. Its acceptor's last handle went with it and the port
    /// is closed for good.
    ServerGone,
    /// The kernel refused for a reason of its own — a full queue, a full table.
    Refused(SyscallError),
}

/// A typed wrapper over a handle it owns, as whoever gave the handle says it
/// is: the kernel refuses each call on a handle of another type, so the type
/// is trusted only as far as the giver's protocol says.
pub trait FromHandle {
    fn from_handle(handle: OwnedHandle) -> Self;
}

macro_rules! from_handle {
    ($($ty:ty => $make:expr),+ $(,)?) => {
        $(impl FromHandle for $ty {
            fn from_handle(handle: OwnedHandle) -> Self {
                let make: fn(OwnedHandle) -> Self = $make;
                make(handle)
            }
        })+
    };
}

from_handle! {
    OwnedHandle => |h| h,
    Acceptor => Acceptor,
    Connector => Connector,
    Namespace => Namespace,
    SysCap => SysCap,
    Job => Job,
    Device => Device,
    crate::Console => crate::Console,
    crate::Pipe => crate::Pipe,
    crate::wake::Waker => crate::wake::Waker::from,
    crate::wake::Bell => crate::wake::Bell::from,
    crate::ipc::Connection => crate::ipc::Connection,
    crate::FramebufferDev => |h| crate::FramebufferDev(Device(h)),
    crate::PciDev => |h| crate::PciDev(Device(h)),
    crate::AcpiDev => |h| crate::AcpiDev(Device(h)),
}

/// This process's endowment table, parsed once.
pub struct Endowments {
    labels: [u8; MAX_LABELS_LEN],
    /// `(label offset, label length)` per entry, and the handle beside it.
    spans: [(u32, u32); MAX_ENDOWMENTS],
    /// **Taken by swap**, so a label answers exactly once: the second `take`
    /// finds [`HANDLE_INVALID`] and returns `None` rather than a second owner
    /// of one handle.
    handles: [AtomicU32; MAX_ENDOWMENTS],
    count: usize,
}

impl Endowments {
    /// This process's own, parsed on first use.
    pub fn get() -> &'static Endowments {
        TABLE.get()
    }

    /// Take the handle labelled `label`, as `T`. `None` once, for a label this
    /// process was not given; `None` again for one already taken.
    pub fn take<T: FromHandle>(&self, label: &str) -> Option<T> {
        let i = self.index_of(label)?;
        let raw = self.handles[i].swap(HANDLE_INVALID.0, Ordering::AcqRel);
        if raw == HANDLE_INVALID.0 {
            return None;
        }
        // The swap is what makes this the only caller to see the handle, and
        // the kernel put it in this process's table at spawn.
        Some(T::from_handle(OwnedHandle(RawHandle(raw))))
    }

    /// Whether `label` names something this process still holds.
    pub fn holds(&self, label: &str) -> bool {
        self.index_of(label)
            .is_some_and(|i| self.handles[i].load(Ordering::Acquire) != HANDLE_INVALID.0)
    }

    /// Every label in the table, taken or not: for a diagnostic, and for a
    /// family of labels such as `toyos::fd`'s, each then taken by name.
    pub fn labels(&self) -> impl Iterator<Item = &str> {
        (0..self.count).map(|i| self.label_at(i))
    }

    fn label_at(&self, i: usize) -> &str {
        let (off, len) = self.spans[i];
        let bytes = &self.labels[off as usize..off as usize + len as usize];
        // The kernel copied these bytes out of what this process's own parent
        // wrote, and a label that is not UTF-8 matches no lookup.
        core::str::from_utf8(bytes).unwrap_or("")
    }

    fn index_of(&self, label: &str) -> Option<usize> {
        (0..self.count).find(|&i| self.label_at(i) == label)
    }
}

/// The process's namespace, or `None` for a program the manifest gives no
/// `receives`.
///
/// Borrowed rather than taken: every `service` call resolves through it and a
/// process has exactly one for its whole life.
pub fn namespace() -> Option<&'static Namespace> {
    NAMESPACE.get().as_ref()
}

/// The namespace holding the supervisor's `launcher` this process was endowed
/// under [`crate::launch::LAUNCHER`], or `None`. Borrowed, as [`namespace`] is:
/// std opens it for every launch, and a parent that hands it on endows a
/// duplicate.
pub fn launcher() -> Option<&'static Namespace> {
    LAUNCHER.get().as_ref()
}

/// This process's handle to itself, which it hands on for a child to be
/// placed under it: `WRITE`, `DUP` and `TRANSFER`, from the kernel, under
/// [`SELF_LABEL`]. Borrowed rather than taken, as the namespace is: every
/// launch duplicates it. Panics for a process whose `self` was taken before
/// this first ran: the kernel starts every process holding it.
pub fn this_process() -> &'static Process {
    THIS_PROCESS.get()
}

/// Make `ns` this process's namespace, for the one process no parent endows
/// one: the supervisor, which builds the machine's ports itself and resolves its own
/// files through them as every program does. Refused when this process already
/// has a namespace or has asked for one.
pub fn adopt_namespace(ns: Namespace) -> Result<(), Namespace> {
    let mut offered = Some(ns);
    NAMESPACE.0.get_or_init(|| offered.take());
    match offered {
        None => Ok(()),
        Some(ns) => Err(ns),
    }
}

/// Open a connection to `name` in this process's namespace.
///
/// The one place a name becomes a connection. It works from the caller's first
/// instruction whether or not the server has reached `accept` or has even been
/// spawned, because the port existed before either process did.
pub fn service(name: &str) -> Result<Connection, EndowError> {
    let ns = namespace().ok_or(EndowError::NotEndowed)?;
    ns.open(name).map_err(|e| match e {
        SyscallError::NotFound => EndowError::NotEndowed,
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

/// Every connector a launching client transferred, taken out of the table.
///
/// **What makes a `provides` name forwardable.** A namespace hands back
/// connections; this hands back the connector itself, so a shell can give its
/// own children the surface its terminal gave it — and it is a `take`, so the
/// caller owns exactly one and duplicates for each child.
pub fn provided(labels: &mut [Option<(&'static str, Connector)>]) -> usize {
    let table = Endowments::get();
    let mut n = 0;
    for i in 0..table.count {
        let label = table.label_at(i);
        let Some(name) = label.strip_prefix(PROVIDE_PREFIX) else { continue };
        if n == labels.len() {
            break;
        }
        let Some(connector) = table.take::<Connector>(label) else { continue };
        labels[n] = Some((name, connector));
        n += 1;
    }
    n
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

/// Compose `<prefix><name>` on the stack. Nothing in the SDK allocates, and a
/// name past the label bound matches nothing rather than being truncated into
/// a label that happens to exist.
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
static NAMESPACE: NamespaceCell = NamespaceCell(Once::new());
static LAUNCHER: LauncherCell = LauncherCell(Once::new());
static THIS_PROCESS: ProcessCell = ProcessCell(Once::new());

struct EndowTable(Once<Endowments>);
struct NamespaceCell(Once<Option<Namespace>>);
struct LauncherCell(Once<Option<Namespace>>);
struct ProcessCell(Once<Process>);

impl EndowTable {
    fn get(&'static self) -> &'static Endowments {
        self.0.get_or_init(read_table)
    }
}

impl NamespaceCell {
    fn get(&'static self) -> &'static Option<Namespace> {
        self.0.get_or_init(|| Endowments::get().take::<Namespace>(SVC_LABEL))
    }
}

impl LauncherCell {
    fn get(&'static self) -> &'static Option<Namespace> {
        self.0.get_or_init(|| Endowments::get().take::<Namespace>(crate::launch::LAUNCHER))
    }
}

impl ProcessCell {
    fn get(&'static self) -> &'static Process {
        self.0.get_or_init(|| {
            Endowments::get().take::<Process>(SELF_LABEL).expect("this process's `self` was taken by another")
        })
    }
}

/// One `SYS_ENDOWMENTS` call: a count, that many entries, then the label blob.
fn read_table() -> Endowments {
    const ENTRY: usize = 16;
    let mut table = Endowments {
        labels: [0; MAX_LABELS_LEN],
        spans: [(0, 0); MAX_ENDOWMENTS],
        handles: [const { AtomicU32::new(HANDLE_INVALID.0) }; MAX_ENDOWMENTS],
        count: 0,
    };
    let mut buf = [0u8; 8 + MAX_ENDOWMENTS * ENTRY + MAX_LABELS_LEN];
    let n = syscall::endowments(&mut buf);
    if n < 8 || n > buf.len() {
        return table;
    }
    let count = u64::from_ne_bytes(buf[..8].try_into().unwrap()) as usize;
    // The kernel bounds this at spawn; a count past it here is a kernel bug,
    // and answering with an empty table would hide it. Fail-fast.
    assert!(count <= MAX_ENDOWMENTS, "the kernel reported {count} endowments");
    let blob = 8 + count * ENTRY;
    assert!(blob <= n, "the endowment table claims more entries than it carries");
    let labels = &buf[blob..n];
    table.labels[..labels.len()].copy_from_slice(labels);
    for i in 0..count {
        let at = 8 + i * ENTRY;
        let off = u32::from_ne_bytes(buf[at..at + 4].try_into().unwrap());
        let len = u32::from_ne_bytes(buf[at + 4..at + 8].try_into().unwrap());
        let handle = u32::from_ne_bytes(buf[at + 8..at + 12].try_into().unwrap());
        table.spans[i] = (off, len);
        table.handles[i].store(handle, Ordering::Relaxed);
    }
    table.count = count;
    table
}
