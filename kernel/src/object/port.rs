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

use crate::pipe::{PipeReader, PipeWriter};
use crate::sync::Lock;
use crate::watch::Watch;

use super::service::HandleQueue;
use super::{KObjectVariant, ObjectCore, ZeroHandles};

/// Unaccepted connections one port may hold; past it, connect returns `ResourceExhausted`.
pub const MAX_PENDING_CONNECTIONS: usize = 32;

/// A connection nobody has accepted yet: owns the server's pipe ends and handle queues.
pub struct PendingConnection {
    pub rx: PipeReader,
    pub tx: PipeWriter,
    pub inbox: Arc<HandleQueue>,
    pub outbox: Arc<HandleQueue>,
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

pub fn create() -> (Arc<Acceptor>, Arc<Connector>) {
    let shared = Arc::new(PortShared {
        id: NEXT_PORT.fetch_add(1, Ordering::Relaxed),
        queue: Lock::new(PortQueue { closed: false, pending: VecDeque::new() }),
        watch: Arc::new(Watch::new()),
    });
    (
        Arc::new(Acceptor { core: Acceptor::new_core(), shared: shared.clone() }),
        Arc::new(Connector { core: Connector::new_core(), shared, badge: None }),
    )
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

    /// A connector to this port whose every connection is stamped with `badge`.
    pub fn mint(&self, badge: Arc<[u8]>) -> Arc<Connector> {
        Arc::new(Connector { core: Connector::new_core(), shared: self.shared.clone(), badge: Some(badge) })
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
        // handle can re-enter another object's hook.
        let queued = {
            let mut queue = self.shared.queue.lock();
            queue.closed = true;
            core::mem::take(&mut queue.pending)
        };
        // Nobody will ever hold this inbox's read end, so a queued
        // `SYS_HANDLE_SEND` on it must say `Gone`, not queue.
        for connection in &queued {
            connection.inbox.close_now();
        }
        drop(queued);
        // A poll on the port is answered as gone, not as ready: nothing will ever queue again.
        self.shared.watch.cancel_polls();
        self.shared.watch.post();
    }
}
