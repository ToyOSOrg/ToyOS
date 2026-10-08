//! [tcp]'s calls on the interface, for `streams`. They reach no address, gateway or resolver: the
//! shard stays `lease`'s to configure.

use toyos_net_shard::ConnectError;
use toyos_net_tcp::{ConnId, Endpoint, Error, Options, Received, Status};
use toyos_net_wire::{Instant, Port};

use super::Stack;

/// [tcp] refuses a caller nothing about a connection that caller holds.
fn held<T>(answer: Result<T, Error>) -> T {
    match answer {
        Ok(value) => value,
        Err(refusal) => unreachable!("[tcp] refused the holder of a connection: {refusal:?}"),
    }
}

impl Stack {
    /// An active open from a port [tcp] draws.
    pub(crate) fn connect(&mut self, now: Instant, remote: Endpoint) -> Result<ConnId, ConnectError> {
        self.shard.connect(now, None, remote)
    }

    pub(crate) fn status(&mut self, id: ConnId) -> Status {
        held(self.shard.status(id))
    }

    pub(crate) fn local_port(&mut self, id: ConnId) -> Port {
        held(self.shard.tuple(id)).local.port
    }

    /// Queues `data`, all of it: the caller offers no more than [`Status::writable`].
    pub(crate) fn stream_send(&mut self, now: Instant, id: ConnId, data: &[u8]) {
        let taken = held(self.shard.send(now, id, data));
        if taken != data.len() {
            unreachable!("[tcp] took {taken} of {} bytes it had room for", data.len());
        }
    }

    /// [`toyos_net_tcp::Tcp::recv_with`].
    pub(crate) fn stream_recv(&mut self, now: Instant, id: ConnId, take: impl FnOnce(&[u8]) -> usize) -> Result<Received, Error> {
        self.shard.recv_with(now, id, take)
    }

    /// The connection's FIN, after what is queued. Of a synchronized connection that has not
    /// failed.
    pub(crate) fn shutdown_write(&mut self, now: Instant, id: ConnId) {
        held(self.shard.shutdown_write(now, id));
    }

    /// The holder lets go, and [tcp] finishes the connection alone; `id` names nothing after.
    pub(crate) fn close(&mut self, now: Instant, id: ConnId) {
        held(self.shard.close(now, id));
    }

    /// RFC 9293 §3.10.5; `id` names nothing after.
    pub(crate) fn abort(&mut self, now: Instant, id: ConnId) {
        held(self.shard.abort(now, id));
    }

    pub(crate) fn set_options(&mut self, now: Instant, id: ConnId, options: Options) {
        held(self.shard.set_options(now, id, options));
    }
}
