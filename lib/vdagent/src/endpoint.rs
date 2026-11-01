//! The agent's port between the virtio-serial driver and the agent, on
//! [`SERVICE`]: the driver holds the function the port is on and no
//! clipboard, the agent the clipboard and no device.
//!
//! **One connection carries the port both ways, in order.** The driver says
//! when the host opens its end ([`HOST_OPENED`]) and closes it
//! ([`HOST_CLOSED`]), and hands over the host's bytes between the two
//! ([`HOST_BYTES`]); the agent's bytes for the host are [`AGENT_BYTES`]. A
//! [`HOST_CLOSED`] is answered with one [`CLOSED`] each: every
//! [`AGENT_BYTES`] before the answer to the driver's last close was a closed
//! session's, and the driver drops them ([`Fence`]), so the host's next
//! session never begins with an earlier one's tail.

/// The port the agent serves and the driver connects to once the device has
/// named the function's port [`crate::PORT_NAME`].
pub const SERVICE: &str = "vdagent";

/// The one row that may receive [`SERVICE`]: whoever holds it speaks as the
/// host to the clipboard's agent.
pub const SERVICE_HOLDER: &str = "virtio-console";

/// Driver → agent, bare: the host opened its end, and a session begins.
pub const HOST_OPENED: u32 = 1;
/// Driver → agent, bare: the host closed its end. Answered with [`CLOSED`].
pub const HOST_CLOSED: u32 = 2;
/// Driver → agent: bytes the host wrote, as the payload.
pub const HOST_BYTES: u32 = 3;
/// Agent → driver: bytes for the host, as the payload.
pub const AGENT_BYTES: u32 = 4;
/// Agent → driver, bare: the answer to [`HOST_CLOSED`].
pub const CLOSED: u32 = 5;

/// What the driver does with a frame the agent sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agent {
    /// Bytes for the host: transmit them.
    Transmit,
    /// Bytes of a session the host has closed: drop them.
    Drop,
    /// The agent's [`CLOSED`] for one of the host's closes.
    Closed,
}

/// A frame the driver does not take from the agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// A type the endpoint does not carry from the agent.
    Unknown(u32),
    /// A [`CLOSED`] for no close the driver told.
    Unasked,
}

/// The driver's side of the sessions: each [`HOST_CLOSED`] it told the agent
/// is answered by one [`CLOSED`], and while any is unanswered, what the agent
/// sends is a closed session's. Counted, because the host may close, open and
/// close again before the agent has read the first close.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Fence {
    unanswered: u32,
}

impl Fence {
    /// The driver told the agent [`HOST_CLOSED`].
    pub fn host_closed(&mut self) {
        self.unanswered += 1;
    }

    /// Whether the agent's frames are still a closed session's, so they are
    /// read whether or not the device has room for them.
    pub fn closing(&self) -> bool {
        self.unanswered > 0
    }

    /// A frame of `msg_type` the agent sent.
    pub fn agent(&mut self, msg_type: u32) -> Result<Agent, Refused> {
        match msg_type {
            AGENT_BYTES if self.closing() => Ok(Agent::Drop),
            AGENT_BYTES => Ok(Agent::Transmit),
            CLOSED if self.closing() => {
                self.unanswered -= 1;
                Ok(Agent::Closed)
            }
            CLOSED => Err(Refused::Unasked),
            other => Err(Refused::Unknown(other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the agent wrote before it learned of the host's close is dropped,
    /// and what it writes after its answer goes to the host's next session.
    #[test]
    fn the_closed_sessions_tail_never_reaches_the_next() {
        let mut fence = Fence::default();
        assert_eq!(fence.agent(AGENT_BYTES), Ok(Agent::Transmit));
        fence.host_closed();
        assert!(fence.closing());
        assert_eq!(fence.agent(AGENT_BYTES), Ok(Agent::Drop));
        assert_eq!(fence.agent(AGENT_BYTES), Ok(Agent::Drop));
        assert_eq!(fence.agent(CLOSED), Ok(Agent::Closed));
        assert!(!fence.closing());
        assert_eq!(fence.agent(AGENT_BYTES), Ok(Agent::Transmit));
    }

    /// The host closes, opens and closes again before the agent answered the
    /// first: what the agent wrote for the session between is that session's,
    /// which the host also closed, so only the second answer ends the drop.
    #[test]
    fn a_session_closed_before_its_predecessors_close_was_answered_is_dropped_too() {
        let mut fence = Fence::default();
        fence.host_closed();
        fence.host_closed();
        assert_eq!(fence.agent(CLOSED), Ok(Agent::Closed));
        assert_eq!(fence.agent(AGENT_BYTES), Ok(Agent::Drop), "the reopened session's bytes, closed since");
        assert_eq!(fence.agent(CLOSED), Ok(Agent::Closed));
        assert_eq!(fence.agent(AGENT_BYTES), Ok(Agent::Transmit));
        assert_eq!(fence.agent(CLOSED), Err(Refused::Unasked));
    }

    /// A frame type the endpoint carries only the other way, or not at all,
    /// is refused by its number.
    #[test]
    fn a_frame_the_agent_does_not_send_is_refused() {
        let mut fence = Fence::default();
        for msg_type in [HOST_OPENED, HOST_CLOSED, HOST_BYTES, 0, 99] {
            assert_eq!(fence.agent(msg_type), Err(Refused::Unknown(msg_type)));
        }
    }
}
