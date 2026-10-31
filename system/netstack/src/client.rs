use std::time::{Duration, Instant};

use toyos::AsHandle;
use toyos::ipc::{self, Connection};
use toyos::net::{ErrorResponse, MAX_REQUEST, RespType};
use toyos::say;

const RESP_RESULT: u32 = RespType::Result as u32;
const RESP_ERROR: u32 = RespType::Error as u32;

/// One client's connection, which is also how netstack names it.
///
/// netstack answers a connection exactly once and then lets it close, so a handler
/// owns this for as long as its operation lasts: the synchronous ones drop it
/// where they answer, and the three asynchronous ones keep it across passes
/// until what they started finishes. The handle closes with it — which is what
/// replaced a `mem::forget` on the accepted connection and eight hand-written
/// `close` calls that had to agree with each other on every path.
pub struct Client {
    pub conn: Connection,
}

impl Client {
    pub fn result_bytes(&self, data: &[u8]) {
        self.answered(self.conn.try_send_bytes(RESP_RESULT, data));
    }

    pub fn error(&self, code: u32) {
        self.answered(self.conn.try_send(RESP_ERROR, &ErrorResponse { code }));
    }

    pub fn snapshot(&self, encoded: &[u8]) {
        self.answered(self.conn.try_send_bytes(toyos_inspect::MSG_SNAPSHOT, encoded));
    }

    /// Whether this client, waiting for its answer, has left. A connection
    /// carries one request, so hanging up is the one thing it may say while
    /// it waits; a client that says more is dropped by name.
    pub fn gone(&self) -> bool {
        match ipc::FrameRx::<0>::new().pump(&self.conn) {
            ipc::RxStep::Idle => false,
            ipc::RxStep::Eof => true,
            ipc::RxStep::Frame { .. } | ipc::RxStep::Malformed => {
                say!("netstack: dropping client {} — it spoke again before its answer", self.conn.as_handle().raw().0);
                true
            }
        }
    }

    /// **The answer goes out in one non-blocking send, and a refusal is not
    /// retried.** `ipc::send` parks in `SYS_MESSAGE_SEND` until the client
    /// drains, which is a client deciding when the network stack runs again.
    /// The connection closes either way.
    /// The log is the only place the machine this runs on gets told that a
    /// client asked something and was never answered.
    fn answered(&self, sent: Result<(), ipc::TrySendError>) {
        if let Err(e) = sent {
            let why = match e {
                ipc::TrySendError::Full => {
                    "its connection has no room for the answer and it is not reading"
                }
                ipc::TrySendError::TooLarge => "the answer netstack built is larger than a frame",
                ipc::TrySendError::Syscall(_) => "its connection is gone",
            };
            say!("netstack: dropping client {} — {why}", self.conn.as_handle().raw().0);
        }
    }
}

/// Connections accepted and not yet carrying a whole request.
///
/// The kernel queues 32 unaccepted connections per listener
/// (`listener::MAX_PENDING_CONNECTIONS`); this is the same allowance one step
/// further along, for a client that has been accepted and has not yet said what
/// it wants. Past it netstack refuses by name rather than growing, and
/// [`HANDSHAKE_TIMEOUT`] is what guarantees the table drains.
pub const MAX_PENDING_CONNS: u32 = 32;

/// How long an accepted connection may go without completing its request.
///
/// Policy, and generous: every client in the tree sends its request in the
/// statement after `connect` (`toyos::net`'s `NetstackConn::request`). What this
/// bounds is the one that never sends it.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);

/// One client's inbound frames, taken without waiting
/// ([`ipc::FrameRx::pump`]): netstack never waits on a client.
pub type ClientRx = ipc::FrameRx<MAX_REQUEST>;

/// A connection that has been accepted and has not yet said what it wants.
///
/// It exists because `accept` and the request frame are two events.
pub struct PendingConn {
    pub conn: Connection,
    pub rx: ClientRx,
    pub since: Instant,
}

/// A whole request, off the connection and in memory.
///
/// The payload travels with the frame instead of being read off the connection
/// during
/// dispatch: the read side is finished before anything acts on a message, so no
/// handler below can park on the client that sent it.
pub struct Request {
    pub client: Client,
    pub msg_type: u32,
    pub payload: [u8; MAX_REQUEST],
    pub payload_len: usize,
    /// What the frame carried: claimed by the request that takes handles, and
    /// closed with any other.
    pub handles: toyos::ipc::Batch,
}

impl Request {
    pub fn payload(&self) -> &[u8] {
        &self.payload[..self.payload_len]
    }
}
