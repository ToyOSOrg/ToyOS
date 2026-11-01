//! Places: the one bound on the streams, listeners and datagram sockets clients make the node
//! hold. A stream holds a place from its connect or its accept until the node lets it go, a
//! listener from its listen and a datagram socket from its bind until its last holder leaves,
//! each holder past a socket's first a place of its own, and a connection the node closed holds
//! the place its stream had until [tcp] has finished it, because its buffers are still the region
//! its client lent and its peer decides for how long, within [tcp]'s bounds on a connection
//! nobody holds. With no place left a connect, a listen, an accept, a bind and an export are
//! refused with nothing made.
//!
//! **The node's own sockets stand outside the places.** The responder's socket for the
//! machine's name and the socket of each query `resolve` has out are bound past
//! [`Node::udp_bind`], the one call in which a datagram socket takes a place, so clients at the
//! bound refuse no lookup and no answer for the name. A lookup is a client's too, and its bound
//! is `resolve`'s own: `toyos_dns::MAX_LOOKUPS` lookups, each with the queries of its rounds.
//!
//! The number is the shell's: each stream is its client's region and its first holder's
//! connection and two bells kept alive and its connection watched, each holder past the first
//! its own connection and bells, and each holder of a listener or a datagram socket its
//! connection and one bell, each priced as a place and one watch.
//! A new node has none.
//!
//! **A place is not one size.** A stream or a connection [tcp] finishes is its client's region,
//! and a datagram socket [udp]'s two queues of `limits::RX_DATAGRAMS` and `limits::TX_DATAGRAMS`
//! datagrams. A listener's place stands for all a peer can make it hold without a client
//! asking, which [tcp] bounds for each listener: `limits::LISTEN_PENDING` handshakes in
//! progress, each given up after `limits::SYNACK_GIVE_UP`, and `limits::LISTEN_READY`
//! connections waiting to be accepted, each with at most one receive buffer of text, taken when
//! its first byte arrives. The shell prices a listener at that product when it picks the number.
//! A connection that waits holds no place of its own: it would take one by a peer's handshake
//! alone, so any peer of any listener could then refuse every client its connect, listen and
//! bind, where now it can fill only the queue of the listener it reaches.

use crate::Node;

impl Node {
    /// How many places the node has from here on. Nothing held is let go for a smaller number.
    pub fn set_places(&mut self, places: usize) {
        self.places = places;
        self.tell_holders();
    }

    /// The places taken: streams, listeners and datagram sockets and their holders past each
    /// first, and connections [tcp] is finishing alone.
    pub fn held(&self) -> usize {
        self.streams()
            .saturating_add(self.streams.more_holders())
            .saturating_add(self.listeners())
            .saturating_add(self.listeners.more_holders())
            .saturating_add(self.datagram_places())
            .saturating_add(self.stack.tcp_orphans())
    }

    pub(crate) fn room(&self) -> usize {
        self.places.saturating_sub(self.held())
    }
}
