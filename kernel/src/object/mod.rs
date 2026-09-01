//! Kernel objects, and the identity every one of them carries.
//!
//! Objects are plain `Arc<T>`: no custom refcounting, no `Weak`, no `dyn` hierarchy.
//!
//! `handle_count`, not the Arc strong count, is what userland-visible lifecycle rides: a syscall's `Arc` can be stranded on a killed thread's kernel stack, so release is deferred through [`ZERO_QUEUE`] — see `issues/kernel/deferred-release-outlives-its-syscall.md`.

// `warn` here gates via CI's `-D warnings`; the rest of the kernel is not yet swept.
#![warn(clippy::undocumented_unsafe_blocks)]

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::num::NonZeroU64;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::sync::Lock;

pub mod device;
pub mod file;
pub mod handle;
pub mod inbox;
pub mod namespace;
pub mod ops;
pub mod pipe;
pub mod port;
pub mod process;
pub mod service;
pub mod shm;
pub mod syscap;

pub use handle::{HandleEntry, HandleError, HandleTable, Refusal};

/// A resource given back on the object's **last handle**, not the last `Arc` — an `Arc` can be stranded on a killed thread's kernel stack.
pub(crate) struct Held<T>(Lock<Option<T>>);

impl<T> Held<T> {
    pub(crate) fn new(value: T) -> Self {
        Self(Lock::new(Some(value)))
    }

    /// Drops what is held, outside the lock; `T`'s destructor must not re-enter it.
    pub(crate) fn release(&self) {
        let taken = self.0.lock().take();
        drop(taken);
    }
}

impl<T: Clone> Held<T> {
    /// A second reference to what is held, or `None` after the last handle released it.
    pub(crate) fn get(&self) -> Option<T> {
        self.0.lock().clone()
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
    koid: Koid,
    /// Table slots, in-flight transfers and spawn endowments — never the `Arc` strong count.
    handle_count: AtomicU32,
    /// A `sealed` row only: set once when its last handle goes, and a second arrival
    /// is a kernel bug caught by `HandleEntry`'s drop assert. Never set on a `reopenable` row.
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

    fn enrol(live: &'static AtomicU64) -> Self {
        live.fetch_add(1, Ordering::Relaxed);
        Self {
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
    /// Runs exactly once, from the deferred queue with no lock held — never
    /// inline at drop, and never taking a sleep lock: no drain site can park.
    fn on_zero_handles(&self);
}

/// One object type's plumbing; implemented only by the [`kobject!`] macro.
pub trait KObjectVariant: ZeroHandles + Send + Sync + Sized + 'static {
    const NAME: &'static str;
    fn from_ref(r: &KObjectRef) -> Option<&Arc<Self>>;
    fn core(&self) -> &ObjectCore;
    /// A fresh core, enrolled in this type's census — the only way to build one.
    fn new_core() -> ObjectCore;
}

/// Declares the closed set of object types; each row says whether its last
/// handle is deferred ([`ZeroHandles`]) or immediate, and whether losing that
/// handle is the object's last name (`sealed`) or not (`reopenable`).
macro_rules! kobject {
    ($($kind:ident $naming:ident $variant:ident => $ty:ty),+ $(,)?) => {
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

            /// Whether something outside every handle table answers for this object.
            fn reopenable(&self) -> bool {
                match self {
                    $(Self::$variant(_) => kobject!(@reopen $naming),)+
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

                fn new_core() -> ObjectCore {
                    ObjectCore::enrol(&census::$variant)
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

            /// `(type name, live count)` per kind, in declaration order — a
            /// summed total would hide one kind's leak in another's churn.
            #[cfg(feature = "test-actuators")]
            pub fn live() -> impl Iterator<Item = (&'static str, u64)> {
                use core::sync::atomic::Ordering;
                [$((stringify!($variant), $variant.load(Ordering::Relaxed)),)+].into_iter()
            }
        }
    };

    (@defers deferred) => { true };
    (@defers immediate) => { false };

    (@reopen sealed) => { false };
    (@reopen reopenable) => { true };

    (@empty_hook deferred $ty:ty) => {};
    (@empty_hook immediate $ty:ty) => {
        impl ZeroHandles for $ty {
            fn on_zero_handles(&self) {}
        }
    };
}

kobject! {
    deferred sealed PipeRead => pipe::PipeReadEnd,
    deferred sealed PipeWrite => pipe::PipeWriteEnd,
    deferred sealed Connection => service::ConnectionEnd,
    deferred sealed Device => device::DeviceClaim,
    deferred sealed Acceptor => port::Acceptor,
    // Kind name is the ABI's; `CENSUS_KIND` asserts the two lists agree.
    deferred sealed Inbox => inbox::InboxObject,
    // The flush that makes releasing it safe waits for every CPU, so it runs from the queue, never inline.
    deferred sealed SharedMem => shm::SharedMemObject,
    // A service with no clients right now is not a service that has stopped.
    immediate sealed Connector => port::Connector,
    // Immutable once built: its `Arc<Connector>`s go with the last reference and nothing observes it.
    immediate sealed Namespace => namespace::Namespace,
    // A file's flush and cache reference ride the last `Arc`; `read`/`write` on a file never park.
    immediate sealed File => file::FileObject,
    immediate sealed Console => device::ConsoleObject,
    // The authority is the rights on the handle; a handle going away *is* the whole event.
    immediate sealed SysCap => syscap::SysCap,
    // The last handle's loss is the loss of the *ability to wait*, not proof the process should stop.
    immediate reopenable Process => process::ProcessObject,
}

/// Objects whose last handle has gone, waiting for release with nothing held;
/// a hook never runs under a lock. One machine-wide queue, not per-CPU: a
/// per-CPU queue would strand work on a CPU that then halts.
static ZERO_QUEUE: Lock<Vec<KObjectRef>> = Lock::new(Vec::new());

/// Whether [`ZERO_QUEUE`] holds anything, readable without the lock; never
/// says empty over a queued object.
static ZERO_PENDING: AtomicBool = AtomicBool::new(false);

pub(crate) fn enqueue_zero_handles(object: KObjectRef) {
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
