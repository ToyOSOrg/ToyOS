//! Places: the one bound on what clients make the node hold. A stream holds a place from its
//! connect or its accept until the node lets it go, a listener from its listen until it is
//! closed, and a connection the node closed holds the place its stream had until [tcp] has
//! finished it, because it is still two buffers of [tcp]'s and its peer decides for how long.
//! With no place left a connect, a listen and an accept are refused with nothing made.
//!
//! The number is the shell's: each stream is two of its client's pipes kept alive and watched,
//! each listener one. A new node has none.
//!
//! What a peer makes the node hold without a client asking is not counted here and is bounded
//! by [tcp] for each listener: `limits::LISTEN_PENDING` handshakes in progress, each given up
//! after `limits::SYNACK_GIVE_UP`, and `limits::LISTEN_READY` connections waiting to be
//! accepted, each with at most one receive buffer of text.

use toyos_net_wire::Instant;

use crate::Node;

impl Node {
    /// How many places the node has from here on. Nothing held is let go for a smaller number.
    pub fn set_places(&mut self, now: Instant, places: usize) {
        self.places = places;
        self.wake_owners(now);
    }

    /// The places taken: streams, listeners, and connections [tcp] is finishing alone.
    pub fn held(&self) -> usize {
        self.streams().saturating_add(self.listeners()).saturating_add(self.stack.tcp_orphans())
    }

    pub(crate) fn room(&self) -> usize {
        self.places.saturating_sub(self.held())
    }
}
