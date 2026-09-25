//! The ring the control thread hands the mix thread.
//!
//! Two threads and one direction: connects, disconnects and volume changes are
//! written by the control thread and drained by the mix thread at the top of
//! every cycle. Nothing travels back — a client's departure reaches the control
//! thread as a dead connection, not as a message.
//!
//! **A server never blocks on a client**, and this ring is where soundd could
//! have. It never drops a command (a ghost client: leaked shm, an app waiting on
//! a stream nothing mixes) and never asserts (soundd dead at a client's
//! choosing); [`submit`] waits instead, throttling the *control* thread, which
//! is the one that can afford it.

use toyos_abi::syscall;
use toyos_abi::RawHandle;
use toyos_mixer::Gain;

use crate::client::{ClientStream, Departure};
use crate::control::MAX_CONTROL_CLIENTS;
use crate::ring::Spsc;

/// Deep enough that one pass of the control loop can never fill it: a pass
/// pushes at most one `AddClient` (there is one accept per wait) plus, per
/// connected client, one coalesced `SetVolume` and one `RemoveClient`.
const CMD_RING_SIZE: usize = 256;
const _: () = assert!(CMD_RING_SIZE >= 1 + 2 * MAX_CONTROL_CLIENTS);

pub(crate) enum MixCommand {
    AddClient(Box<ClientStream>),
    RemoveClient { client_id: usize, departure: Departure },
    SetVolume { client_id: usize, target: Gain },
}

/// The control thread is the one producer and the mix thread the one consumer.
pub(crate) struct CommandRing(Spsc<MixCommand, CMD_RING_SIZE>);

unsafe impl Send for CommandRing {}
unsafe impl Sync for CommandRing {}

impl CommandRing {
    pub(crate) fn new() -> Self {
        Self(Spsc::new())
    }

    /// Hands the command back when the ring is full rather than dropping it (a
    /// ghost client: leaked shm, an app waiting on a stream nothing mixes) or
    /// asserting — a client chooses the load, since the control thread drains
    /// everything it has written before yielding. See `submit`, which waits.
    #[must_use]
    fn try_push(&self, cmd: MixCommand) -> Result<(), MixCommand> {
        self.0.try_push(cmd)
    }

    pub(crate) fn pop(&self) -> Option<MixCommand> {
        self.0.pop()
    }
}

/// Hand one command to the mix thread, waiting for room if the ring is full.
///
/// The mix thread drains the whole ring at the top of every cycle, so a full
/// ring means it has not run for a cycle and one device period is exactly how
/// long there is to wait. Throttling the control thread is the point: the
/// alternatives are dropping a command (a client stranded in the mix thread
/// forever) or asserting (soundd dead at a client's choosing). The retry is
/// unbounded because the mix thread is the process's main thread — if it has
/// stopped, soundd is already gone.
pub(crate) fn submit(cmd_ring: &CommandRing, cmd_pipe_write: RawHandle, cmd: MixCommand, period_nanos: u64) {
    let mut cmd = cmd;
    loop {
        let full = cmd_ring.try_push(cmd);
        let _ = syscall::write_nonblock(cmd_pipe_write, &[1]);
        match full {
            Ok(()) => return,
            Err(returned) => {
                cmd = returned;
                syscall::nanosleep(period_nanos);
            }
        }
    }
}

/// Tell the mix thread a stream ended, and how.
///
/// Every removal the control thread issues goes through here, so the witness it
/// holds — which of the four ways this stream ended — travels with the command
/// instead of being reconstructed from a flag on the other side.
pub(crate) fn remove(
    cmd_ring: &CommandRing,
    cmd_pipe_write: RawHandle,
    client_id: usize,
    departure: Departure,
    period_nanos: u64,
) {
    submit(
        cmd_ring,
        cmd_pipe_write,
        MixCommand::RemoveClient { client_id, departure },
        period_nanos,
    );
}
