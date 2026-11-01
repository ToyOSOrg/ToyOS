//! Asking `logkeeper` for this boot's log on this machine: one [`READ`] on
//! its [`SERVICE`] port, answered by one [`SERVED`] frame carrying how many
//! bytes are the boot so far and the read end of a pipe the log is written
//! into, from its first line.

use toyos::endow;
use toyos::Pipe;
use toyos_logstream::{READ, SERVED, SERVICE};

/// `logkeeper`'s answer.
pub struct Served {
    pub pipe: Pipe,
    /// How many of the pipe's bytes are the boot so far.
    pub boot_so_far: u64,
}

/// Ask: one request, and a blocking read of its one answer.
pub fn read() -> Result<Served, String> {
    let conn = endow::service(SERVICE).map_err(|e| format!("no `{SERVICE}` service: {e:?}"))?;
    conn.signal(READ).map_err(|e| format!("logkeeper would not take the request: {e:?}"))?;
    let mut rx = toyos::ipc::FrameRx::<8>::new();
    let frame = rx.recv(&conn).map_err(|e| format!("logkeeper did not answer: {e:?}"))?;
    if frame.msg_type != SERVED {
        return Err(format!("logkeeper answered frame type {}", frame.msg_type));
    }
    let boot_so_far: u64 = toyos::ipc::decode_payload(rx.payload(frame.payload_len))
        .map_err(|e| format!("logkeeper's answer is not one `u64`: {e:?}"))?;
    let [pipe] = rx.take_handles::<1>().ok_or("logkeeper's answer carried no pipe")?;
    let pipe = Pipe::from(pipe);
    Ok(Served { pipe, boot_so_far })
}
