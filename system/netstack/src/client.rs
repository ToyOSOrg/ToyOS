use std::time::{Duration, Instant};

use toyos::AsHandle;
use toyos::ipc::{self, Connection, IpcPayload};
use toyos::net::{ErrorResponse, RespType};
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
    pub fn result<T: IpcPayload>(&self, payload: &T) {
        self.answered(self.conn.try_send(RESP_RESULT, payload));
    }

    pub fn result_bytes(&self, data: &[u8]) {
        self.answered(self.conn.try_send_bytes(RESP_RESULT, data));
    }

    pub fn done(&self) {
        self.answered(self.conn.try_signal(RESP_RESULT));
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
        let mut byte = [0u8; 1];
        match self.conn.read_nonblock(&mut byte) {
            Err(toyos_abi::syscall::SyscallError::WouldBlock) => false,
            Ok(0) | Err(_) => true,
            Ok(_) => {
                say!("netstack: dropping client {} — it spoke again before its answer", self.conn.as_handle().0);
                true
            }
        }
    }

    /// **The answer goes out in one non-blocking write, and a refusal is not
    /// retried.** `ipc::send` parks in `sys_write` until the client drains,
    /// which is a client deciding when the network stack runs again; and
    /// `TrySendError::Full` can have left part of the frame in the pipe, so
    /// there is nothing here to retry either. The connection closes either way.
    /// The log is the only place the machine this runs on gets told that a
    /// client asked something and was never answered.
    fn answered(&self, sent: Result<(), ipc::TrySendError>) {
        if let Err(e) = sent {
            let why = match e {
                ipc::TrySendError::Full => {
                    "its pipe will not take the answer and it is not reading"
                }
                ipc::TrySendError::TooLarge => "the answer netstack built is larger than a frame",
                ipc::TrySendError::Syscall(_) => "its connection is gone",
            };
            say!("netstack: dropping client {} — {why}", self.conn.as_handle().0);
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

/// The largest request payload netstack keeps.
///
/// `MsgType::DnsLookup` is the only request carrying bytes rather than a struct,
/// and `toyos::net::dns_lookup` frames a hostname into a 256-byte buffer; every
/// typed request is far smaller, `TcpConnectPipedRequest` at 32 bytes being the
/// widest. A client may declare anything up to `ipc::MAX_FRAME_LEN` — the excess
/// is counted down and discarded, never waited for.
pub const MAX_KEPT_REQUEST: usize = 256;

/// One client's inbound framing.
///
/// **netstack never reads a client with a blocking read.** That is the whole point
/// of [`ipc::FrameRx`]: `ipc::recv_header` and `ipc::recv_payload` park the
/// caller until the peer sends the bytes it promised. Here a peer that stops halfway
/// through a frame costs a buffer and a deadline instead of the event loop.
pub type ClientRx = ipc::FrameRx<MAX_KEPT_REQUEST>;

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
    pub payload: [u8; MAX_KEPT_REQUEST],
    pub payload_len: usize,
}

impl Request {
    pub fn payload(&self) -> &[u8] {
        &self.payload[..self.payload_len]
    }
}
