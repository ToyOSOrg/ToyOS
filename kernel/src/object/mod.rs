//! Kernel objects, and the identity every one of them carries.
//!
//! Objects are plain `Arc<T>`: no custom refcounting, no `Weak`, no `dyn` hierarchy.
//!
//! An object ends with its last handle and is never handed out again: a holder that answers more than once keeps what an object is made over (`device::Screen`, a claim's BAR windows) and makes a fresh object for each answer.
//!
//! `handle_count`, not the Arc strong count, is what userland-visible lifecycle rides: a syscall's `Arc` can be stranded on a killed thread's kernel stack, so release is deferred: through a dying process's own [`Releases`], run before its end is published, or else through [`ZERO_QUEUE`] — see `issues/deferred-release-outlives-its-syscall.md`.

// `warn` here gates via CI's `-D warnings`; the rest of the kernel is not yet swept.
#![warn(clippy::undocumented_unsafe_blocks)]

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::num::NonZeroU64;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::mm::charge::{self, Charge, Exhausted};
use crate::sync::Lock;

pub mod device;
pub mod devset;
pub mod event;
pub mod file;
pub mod handle;
pub mod inbox;
pub mod job;
pub mod namespace;
pub mod notice;
pub mod null;
pub mod ops;
pub mod paged;
pub mod pipe;
pub mod port;
pub mod process;
mod releases;
pub mod service;
pub mod shm;
pub mod syscap;
pub mod wake_latency;

pub use handle::{HandleEntry, HandleError, HandleTable, Refusal};
pub(crate) use releases::{Released, Releases};

/// A resource given back on the object's **last handle**, not the last `Arc` — an `Arc` can be stranded on a killed thread's kernel stack.
pub(crate) struct Held<T>(Lock<Option<T>>);

impl<T> Held<T> {
    pub(crate) fn new(value: T) -> Self {
        Self(Lock::new(Some(value)))
    }

    /// What is held, for a release that is more than a drop; `None` after the first.
    pub(crate) fn take(&self) -> Option<T> {
        self.0.lock().take()
    }

    /// Drops what is held, outside the lock; `T`'s destructor must not re-enter it.
    pub(crate) fn release(&self) {
        drop(self.take());
    }

    /// `f` over what is held, under the lock, or `None` after the release.
    pub(crate) fn with<R>(&self, f: impl FnOnce(&T) -> R) -> Option<R> {
        self.0.lock().as_ref().map(f)
    }

    /// [`with`](Self::with) for an `f` that changes what is held: the change and the release
    /// are one lock's, so nothing is added to what a release already took.
    pub(crate) fn with_mut<R>(&self, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        self.0.lock().as_mut().map(f)
    }
}

/// A kernel object's identity: diagnostic and internal-key only, never an authority — no syscall turns a koid into access.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Koid(NonZeroU64);

impl Koid {
    pub fn raw(self) -> u64 {
        self.0.get()
    }

    fn mint() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let raw = NEXT.fetch_add(1, Ordering::Relaxed);
        Self(NonZeroU64::new(raw).expect("koid counter wrapped through zero"))
    }
}

/// What every object embeds; built only by the macro-generated `core()`, so
/// nothing is constructed outside the census.
pub struct ObjectCore {
    /// The object's own allocation and what its constructor said it keeps besides.
    _charge: Charge,
    koid: Koid,
    /// Table slots, in-flight transfers and spawn endowments — never the `Arc` strong count.
    handle_count: AtomicU32,
    /// Set once; a second arrival is a kernel bug caught by `HandleEntry`'s drop assert.
    retired: AtomicBool,
    /// This type's census counter; decremented by `ObjectCore`'s own drop.
    live: &'static AtomicU64,
}

impl ObjectCore {
    pub fn koid(&self) -> Koid {
        self.koid
    }

    pub fn retired(&self) -> bool {
        self.retired.load(Ordering::Acquire)
    }

    /// The job the object is charged to: the caller's that made it, or the
    /// one it was made for ([`KObjectVariant::new_core_for`]).
    pub fn job(&self) -> &Arc<kernel::sched::job::Job> {
        self._charge.job()
    }

    fn enrol(live: &'static AtomicU64, charge: Charge) -> Self {
        live.fetch_add(1, Ordering::Relaxed);
        Self {
            _charge: charge,
            koid: Koid::mint(),
            handle_count: AtomicU32::new(0),
            retired: AtomicBool::new(false),
            live,
        }
    }
}

impl Drop for ObjectCore {
    fn drop(&mut self) {
        self.live.fetch_sub(1, Ordering::Relaxed);
    }
}

/// What an object does when its last handle goes; a `deferred` row must
/// implement it, `immediate` gets an empty impl from the macro.
pub trait ZeroHandles {
    /// Runs exactly once with no lock held, from the deferred queue or a dying
    /// process's [`Releases`] — never inline at drop, and never taking a sleep
    /// lock: neither can park.
    fn on_zero_handles(&self);
}

/// One object type's plumbing; implemented only by the [`kobject!`] macro.
pub trait KObjectVariant: ZeroHandles + Send + Sync + Sized + 'static {
    const NAME: &'static str;
    fn from_ref(r: &KObjectRef) -> Option<&Arc<Self>>;
    fn core(&self) -> &ObjectCore;
    /// A fresh core, enrolled in this type's census — the only way to build
    /// one — charged to the caller's job the object's own `Arc` and the
    /// `extra` heap bytes its constructor keeps besides; refused before
    /// anything is allocated.
    fn new_core(extra: usize) -> Result<ObjectCore, Exhausted> {
        Self::new_core_for(&charge::current(), extra)
    }

    /// [`new_core`](Self::new_core), charged to `job`.
    fn new_core_for(job: &Arc<kernel::sched::job::Job>, extra: usize) -> Result<ObjectCore, Exhausted>;
}

/// Declares the closed set of object types; each row says whether its last
/// handle is deferred ([`ZeroHandles`]) or immediate.
macro_rules! kobject {
    ($($kind:ident $variant:ident => $ty:ty),+ $(,)?) => {
        /// Every kind of thing a handle can name; matched exhaustively with no
        /// wildcard arm, so a new row is a compile error at every dispatch site.
        #[derive(Clone)]
        pub enum KObjectRef {
            $($variant(Arc<$ty>),)+
        }

        impl KObjectRef {
            pub fn core(&self) -> &ObjectCore {
                match self {
                    $(Self::$variant(o) => o.core(),)+
                }
            }

            /// This object's type, for a `WrongType` refusal that names both.
            pub fn kind(&self) -> &'static str {
                match self {
                    $(Self::$variant(_) => <$ty as KObjectVariant>::NAME,)+
                }
            }

            fn run_zero_handles(&self) {
                match self {
                    $(Self::$variant(o) => o.on_zero_handles(),)+
                }
            }

            /// Whether releasing this object waits for the deferred queue.
            fn defers_release(&self) -> bool {
                match self {
                    $(Self::$variant(_) => kobject!(@defers $kind),)+
                }
            }
        }

        $(
            kobject!(@empty_hook $kind $ty);

            impl KObjectVariant for $ty {
                const NAME: &'static str = stringify!($variant);

                fn from_ref(r: &KObjectRef) -> Option<&Arc<Self>> {
                    match r {
                        KObjectRef::$variant(o) => Some(o),
                        // Reachable for every variant but the first; the macro can't know how many rows follow.
                        #[allow(unreachable_patterns)]
                        _ => None,
                    }
                }

                fn core(&self) -> &ObjectCore {
                    &self.core
                }

                fn new_core_for(job: &Arc<kernel::sched::job::Job>, extra: usize) -> Result<ObjectCore, Exhausted> {
                    let charge = Charge::new(job, charge::arc::<$ty>().saturating_add(extra))?;
                    Ok(ObjectCore::enrol(&census::$variant, charge))
                }
            }
        )+

        /// Live counts per object type; the only detector for a stranded-`Arc`
        /// or connection-cycle leak.
        pub mod census {
            use core::sync::atomic::AtomicU64;

            $(
                #[allow(non_upper_case_globals)]
                pub(super) static $variant: AtomicU64 = AtomicU64::new(0);
            )+

            /// Live objects of one kind — a summed total would hide one
            /// kind's leak in another's churn.
            #[cfg(feature = "test-actuators")]
            pub fn live(kind: toyos_abi::handle::ObjectKind) -> u64 {
                use core::sync::atomic::Ordering;
                match kind {
                    $(toyos_abi::handle::ObjectKind::$variant => $variant.load(Ordering::Relaxed),)+
                }
            }
        }
    };

    (@defers deferred) => { true };
    (@defers immediate) => { false };

    (@empty_hook deferred $ty:ty) => {};
    (@empty_hook immediate $ty:ty) => {
        impl ZeroHandles for $ty {
            fn on_zero_handles(&self) {}
        }
    };
}

kobject! {
    deferred PipeRead => pipe::PipeReadEnd,
    deferred PipeWrite => pipe::PipeWriteEnd,
    deferred Connection => service::ConnectionEnd,
    deferred Device => device::DeviceClaim,
    deferred Acceptor => port::Acceptor,
    deferred Inbox => inbox::InboxObject,
    // Each side's last handle is that side's end, which the other side reads.
    deferred EventSignal => event::EventSignal,
    deferred EventWait => event::EventWait,
    // The flush that makes releasing it safe waits for every CPU, so it runs from the queue, never inline.
    deferred SharedMem => shm::SharedMemObject,
    // A service with no clients right now is not a service that has stopped.
    immediate Connector => port::Connector,
    // Immutable once built: its `Arc<Connector>`s go with the last reference and nothing observes it.
    immediate Namespace => namespace::Namespace,
    // A file's flush and cache reference ride the last `Arc`; `read`/`write` on a file never park.
    immediate File => file::FileObject,
    immediate Console => device::ConsoleObject,
    // The authority is the rights on the handle; a handle going away *is* the whole event.
    immediate SysCap => syscap::SysCap,
    // The last handle's loss is the loss of the *ability to wait*, not proof the process should stop.
    immediate Process => process::ProcessObject,
    immediate Null => null::NullObject,
    // A job outlives its handles while a thread of it runs: what ends it is its last reference.
    immediate Job => job::JobObject,
    // Its one handle is its process's, and goes with it.
    immediate Notice => notice::NoticeObject,
    // What it holds is released by its last reference: nothing waits on it.
    immediate WakeLatency => wake_latency::WakeLatencyObject,
    // Its one handle is its process's: its last ends every object it serves.
    deferred Pager => paged::PagerObject,
    // Its last handle may be its object's last holder, which its pager is told.
    deferred Paged => paged::PagedHolderObject,
    // A view of the kernel's set: losing it changes nothing in the set.
    immediate DeviceSet => devset::DeviceSet,
}

/// Objects whose last handle has gone, waiting for release with nothing held;
/// a hook never runs under a lock. One machine-wide queue, not per-CPU: a
/// per-CPU queue would strand work on a CPU that then halts.
static ZERO_QUEUE: Lock<Vec<KObjectRef>> = Lock::new(Vec::new());

/// Whether [`ZERO_QUEUE`] holds anything, readable without the lock; never
/// says empty over a queued object.
static ZERO_PENDING: AtomicBool = AtomicBool::new(false);

/// A deferred object whose last handle has gone: owed to the running thread's
/// open [`Releases`], or else to [`ZERO_QUEUE`].
pub(crate) fn enqueue_zero_handles(object: KObjectRef) {
    let Some(object) = releases::collect(object) else { return };
    let mut queue = ZERO_QUEUE.lock();
    queue.push(object);
    ZERO_PENDING.store(true, Ordering::Release);
}

/// Runs the hooks of every object whose last handle has gone; called at
/// syscall exit, each scheduler pass and from idle. Another CPU's drain can
/// take a batch a syscall queued, so that syscall can return to userland
/// before its own releases finish.
pub fn drain_zero_handles() {
    while ZERO_PENDING.load(Ordering::Acquire) {
        // A hook may retire further objects, so this repeats until the queue stays empty.
        let batch = {
            let mut queue = ZERO_QUEUE.lock();
            // Cleared before hooks run: work a hook enqueues here is not lost
            // when the loop above rechecks the flag.
            ZERO_PENDING.store(false, Ordering::Release);
            core::mem::take(&mut *queue)
        };
        for object in batch {
            object.run_zero_handles();
        }
    }
}
