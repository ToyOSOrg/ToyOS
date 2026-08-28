//! A port: two object types, [`Acceptor`] and [`Connector`], over one shared
//! connection queue. Both ends are created together before either process
//! runs, so a client's first connect always has something to reach — never a
//! name that is not yet bound, so nothing to retry and no timeout.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;


use crate::inbox::InboxId;
use crate::pipe::{PipeReader, PipeWriter};
use crate::sync::Lock;

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
}

/// `closed` and `pending` share one lock: checking `closed` and pushing must not
/// interleave, or a connection queues after nothing will ever drain it again.
struct PortQueue {
    closed: bool,
    pending: VecDeque<PendingConnection>,
}

/// Everything the two ends share; neither end holds the other, so no `Arc` cycle exists.
pub struct PortShared {
    queue: Lock<PortQueue>,
    /// Lives on the port, not either end: a client's connect must complete a
    /// poll the server registered on the `Acceptor`.
    watch: crate::completion::Watch,
    inbox_watchers: Lock<Vec<InboxId>>,
}

pub struct Acceptor {
    pub(super) core: ObjectCore,
    shared: Arc<PortShared>,
}

pub struct Connector {
    pub(super) core: ObjectCore,
    shared: Arc<PortShared>,
}

/// Why a connection was not queued.
pub enum PushError {
    /// The acceptor is gone: the server exited, or never existed.
    Closed,
    QueueFull,
}

pub fn create() -> (Arc<Acceptor>, Arc<Connector>) {
    let shared = Arc::new(PortShared {
        queue: Lock::new(PortQueue { closed: false, pending: VecDeque::new() }),
        watch: crate::completion::Watch::new(),
        inbox_watchers: Lock::new(Vec::new()),
    });
    (
        Arc::new(Acceptor { core: Acceptor::new_core(), shared: shared.clone() }),
        Arc::new(Connector { core: Connector::new_core(), shared }),
    )
}

impl PortShared {
    pub fn has_pending(&self) -> bool {
        !self.queue.lock().pending.is_empty()
    }

    fn closed(&self) -> bool {
        self.queue.lock().closed
    }

    pub fn watch(&self) -> &crate::completion::Watch {
        &self.watch
    }

    pub fn watchers(&self) -> Vec<InboxId> {
        self.inbox_watchers.lock().clone()
    }

    pub fn add_watcher(&self, ring: InboxId) {
        let mut watchers = self.inbox_watchers.lock();
        if !watchers.contains(&ring) {
            watchers.push(ring);
        }
    }

    pub fn remove_watcher(&self, ring: InboxId) {
        self.inbox_watchers.lock().retain(|&id| id != ring);
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

    pub fn watch(&self) -> &crate::completion::Watch {
        self.shared.watch()
    }

    pub fn port(&self) -> Arc<PortShared> {
        self.shared.clone()
    }
}

impl Connector {
    pub fn closed(&self) -> bool {
        self.shared.closed()
    }

    /// One lock acquisition for the check and the insert; see [`PortQueue`].
    pub fn push(&self, connection: PendingConnection) -> Result<(), PushError> {
        let mut queue = self.shared.queue.lock();
        if queue.closed {
            return Err(PushError::Closed);
        }
        if queue.pending.len() >= MAX_PENDING_CONNECTIONS {
            return Err(PushError::QueueFull);
        }
        queue.pending.push_back(connection);
        Ok(())
    }

    pub fn port(&self) -> Arc<PortShared> {
        self.shared.clone()
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
        crate::completion::post(
            crate::completion::Subject::of(self.shared.watch()),
            crate::completion::Outcome::Gone(crate::completion::Reason::Closed),
        );
    }
}
