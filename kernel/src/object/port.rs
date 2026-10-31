//! A port: two object types, [`Acceptor`] and [`Connector`], over one shared
//! connection queue. Both ends are created together before either process
//! runs, so a client's first connect always has something to reach — never a
//! name that is not yet bound, so nothing to retry and no timeout.
//!
//! **The kernel vouches for what the acceptor's holder granted, never for who
//! connected.** The holder mints a connector carrying bytes it chose
//! ([`Acceptor::mint`]); a connection made through it is stamped with them and
//! the port, and only that port's acceptor reads the stamp back.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::mm::charge::{self, Exhausted};
use crate::message::{Receiver, Sender};
use crate::sync::Lock;
use crate::watch::Watch;

use super::{KObjectVariant, ObjectCore, ZeroHandles};

/// Unaccepted connections one port may hold; past it, connect returns `ResourceExhausted`.
pub const MAX_PENDING_CONNECTIONS: usize = 32;

/// A connection nobody has accepted yet: owns the server's ends of its two
/// directions.
pub struct PendingConnection {
    pub rx: Receiver,
    pub tx: Sender,
    /// [`Connector::stamp`] of the connector the client connected through.
    pub stamp: Stamp,
}

/// What an accepted connection keeps of the connector it was made through.
#[derive(Clone)]
pub struct Stamp {
    /// The port's never-repeating id, which the acceptor asking for the badge must match.
    pub port: u64,
    pub badge: Option<Arc<[u8]>>,
}

/// The heap a badge takes, which each holder of a stamp of it is charged
/// besides the connector it was minted on: the badge goes only with the last.
fn badge_heap(badge: &Arc<[u8]>) -> usize {
    let (inner, _) = core::alloc::Layout::new::<[usize; 2]>()
        .extend(core::alloc::Layout::for_value::<[u8]>(badge))
        .expect("port: a badge's layout");
    charge::heap(inner.pad_to_align())
}

/// Never repeats, so a stamp outlives its port without ever naming another one.
static NEXT_PORT: AtomicU64 = AtomicU64::new(1);

/// `closed` and `pending` share one lock: checking `closed` and pushing must not
/// interleave, or a connection queues after nothing will ever drain it again.
struct PortQueue {
    closed: bool,
    pending: VecDeque<PendingConnection>,
}

/// Everything the two ends share; neither end holds the other, so no `Arc` cycle exists.
pub struct PortShared {
    id: u64,
    queue: Lock<PortQueue>,
    /// Lives on the port, not either end: a client's connect must complete a
    /// poll the server registered on the `Acceptor`. An `Arc` so a poll
    /// registration can hold it with no end borrowed.
    watch: Arc<Watch>,
}

pub struct Acceptor {
    pub(super) core: ObjectCore,
    shared: Arc<PortShared>,
}

pub struct Connector {
    pub(super) core: ObjectCore,
    shared: Arc<PortShared>,
    /// Immutable for the object's life: a duplicate is another handle to this object.
    badge: Option<Arc<[u8]>>,
}

/// Why a connection was not queued.
pub enum PushError {
    /// The acceptor is gone: the server exited, or never existed.
    Closed,
    QueueFull,
}

/// A port's two ends. The acceptor is charged what they share, its queue at
/// its bound included, and each connector itself.
pub fn create() -> Result<(Arc<Acceptor>, Arc<Connector>), Exhausted> {
    let shared = charge::arc::<PortShared>()
        + charge::arc::<Watch>()
        + charge::array::<PendingConnection>(MAX_PENDING_CONNECTIONS.next_power_of_two());
    let acceptor = Acceptor::new_core(shared)?;
    let connector = Connector::new_core(0)?;
    let shared = Arc::new(PortShared {
        id: NEXT_PORT.fetch_add(1, Ordering::Relaxed),
        queue: Lock::new(PortQueue { closed: false, pending: VecDeque::new() }),
        watch: Arc::new(Watch::new()),
    });
    Ok((
        Arc::new(Acceptor { core: acceptor, shared: shared.clone() }),
        Arc::new(Connector { core: connector, shared, badge: None }),
    ))
}

impl PortShared {
    pub fn has_pending(&self) -> bool {
        !self.queue.lock().pending.is_empty()
    }

    fn closed(&self) -> bool {
        self.queue.lock().closed
    }

    pub fn watch(&self) -> &Arc<Watch> {
        &self.watch
    }
}

impl Acceptor {
    pub fn pop(&self) -> Option<PendingConnection> {
        self.shared.queue.lock().pending.pop_front()
    }

    /// True once nothing will ever be queued again.
    pub fn closed(&self) -> bool {
        self.shared.closed()
    }

    pub fn has_pending(&self) -> bool {
        self.shared.has_pending()
    }

    pub fn watch(&self) -> &Arc<Watch> {
        self.shared.watch()
    }

    pub fn port_id(&self) -> u64 {
        self.shared.id
    }

    /// A connector to this port whose every connection is stamped with
    /// `badge`, charged for it.
    pub fn mint(&self, badge: Arc<[u8]>) -> Result<Arc<Connector>, Exhausted> {
        let core = Connector::new_core(badge_heap(&badge))?;
        Ok(Arc::new(Connector { core, shared: self.shared.clone(), badge: Some(badge) }))
    }
}

impl Connector {
    pub fn closed(&self) -> bool {
        self.shared.closed()
    }

    /// One lock acquisition for the check and the insert; see [`PortQueue`].
    /// A refusal hands the connection back: its caller holds a handle table's
    /// lock, and the pipe ends are not dropped under one.
    pub fn push(&self, connection: PendingConnection) -> Result<(), (PendingConnection, PushError)> {
        let mut queue = self.shared.queue.lock();
        if queue.closed {
            return Err((connection, PushError::Closed));
        }
        if queue.pending.len() >= MAX_PENDING_CONNECTIONS {
            return Err((connection, PushError::QueueFull));
        }
        queue.pending.push_back(connection);
        Ok(())
    }

    pub fn port(&self) -> Arc<PortShared> {
        self.shared.clone()
    }

    /// What a connection made through this connector is stamped with.
    pub fn stamp(&self) -> Stamp {
        Stamp { port: self.shared.id, badge: self.badge.clone() }
    }
}

/// Wakes every thread parked in `accept` and drops each queued connection's
/// pipe ends, so a blocked client's next write is `Gone` and its next read `0`.
impl ZeroHandles for Acceptor {
    fn on_zero_handles(&self) {
        // `queued` drops only after the guard releases, since dropping a
        // handle can re-enter another object's hook. Its receivers go with
        // it, so a client's send on a connection nobody accepted is `Gone`.
        let queued = {
            let mut queue = self.shared.queue.lock();
            queue.closed = true;
            core::mem::take(&mut queue.pending)
        };
        drop(queued);
        // A poll on the port is answered as gone, not as ready: nothing will ever queue again.
        self.shared.watch.cancel_polls();
        self.shared.watch.post();
    }
}
