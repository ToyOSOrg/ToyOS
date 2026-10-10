//! [tcp]'s calls on the interface, for `streams` and `listeners`. They reach no address, gateway
//! or resolver: the shard stays `lease`'s to configure.

use core::net::Ipv4Addr;

use toyos_net_shard::{ConnectError, ListenError};
use toyos_net_tcp::{ConnId, Endpoint, Error, ListenerId, Options, Received, Status, Tuple};
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
    pub(crate) fn tcp_connect(&mut self, now: Instant, remote: Endpoint) -> Result<ConnId, ConnectError> {
        self.shard.connect(now, None, remote)
    }

    /// MEASUREMENT ONLY.
    pub(crate) fn tcp_info(&mut self, id: ConnId) -> Option<toyos_net_tcp::Info> {
        self.shard.tcp_info(id)
    }

    pub(crate) fn tcp_status(&mut self, id: ConnId) -> Status {
        held(self.shard.status(id))
    }

    pub(crate) fn tcp_local_port(&mut self, id: ConnId) -> Port {
        held(self.shard.tuple(id)).local.port
    }

    /// Queues `data`, all of it: the caller offers no more than [`Status::writable`].
    pub(crate) fn tcp_send(&mut self, now: Instant, id: ConnId, data: &[u8]) {
        let taken = held(self.shard.send(now, id, data));
        if taken != data.len() {
            unreachable!("[tcp] took {taken} of {} bytes it had room for", data.len());
        }
    }

    /// [`toyos_net_tcp::Tcp::recv_with`].
    pub(crate) fn tcp_recv(&mut self, now: Instant, id: ConnId, take: impl FnOnce(&[u8]) -> usize) -> Result<Received, Error> {
        self.shard.recv_with(now, id, take)
    }

    /// The connection's FIN, after what is queued. Of a synchronized connection that has not
    /// failed.
    pub(crate) fn tcp_shutdown_write(&mut self, now: Instant, id: ConnId) {
        held(self.shard.shutdown_write(now, id));
    }

    /// The holder lets go, and [tcp] finishes the connection alone; `id` names nothing after.
    pub(crate) fn tcp_close(&mut self, now: Instant, id: ConnId) {
        held(self.shard.close(now, id));
    }

    /// RFC 9293 §3.10.5; `id` names nothing after.
    pub(crate) fn tcp_abort(&mut self, now: Instant, id: ConnId) {
        held(self.shard.abort(now, id));
    }

    pub(crate) fn tcp_set_options(&mut self, now: Instant, id: ConnId, options: Options) {
        held(self.shard.set_options(now, id, options));
    }

    /// Connections `close` left [tcp] to finish alone, each until it ends.
    pub(crate) fn tcp_orphans(&self) -> usize {
        self.shard.orphans()
    }

    /// A passive open at `addr`, 0.0.0.0 meaning every address the interface holds or comes to
    /// hold, and at `port` or at one of `random`'s candidates.
    pub(crate) fn tcp_listen(&mut self, addr: Ipv4Addr, port: Option<Port>, random: impl FnMut() -> u16) -> Result<(ListenerId, Port), ListenError> {
        let id = self.shard.listen(addr, port, random)?;
        Ok((id, held(self.shard.listener_port(id))))
    }

    /// What a connection whose SYN arrives at `id` from here on starts with.
    pub(crate) fn tcp_set_listener_options(&mut self, id: ListenerId, options: Options) {
        held(self.shard.set_listener_options(id, options));
    }

    /// How many connections finished their handshake at `id` and wait to be accepted.
    pub(crate) fn tcp_ready(&mut self, id: ListenerId) -> usize {
        held(self.shard.ready(id))
    }

    /// The oldest connection waiting at `id`, the caller's from here, its two endpoints and the
    /// options it has.
    pub(crate) fn tcp_accept(&mut self, id: ListenerId) -> Option<(ConnId, Tuple, Options)> {
        let conn = held(self.shard.accept(id))?;
        Some((conn, held(self.shard.tuple(conn)), held(self.shard.options(conn))))
    }

    /// [tcp] resets every connection still waiting at `id`; `id` names nothing after.
    pub(crate) fn tcp_close_listener(&mut self, now: Instant, id: ListenerId) {
        held(self.shard.close_listener(now, id));
    }
}
