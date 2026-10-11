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
    let header = conn.recv_header().map_err(|e| format!("logkeeper did not answer: {e:?}"))?;
    if header.msg_type != SERVED {
        return Err(format!("logkeeper answered frame type {}", header.msg_type));
    }
    let boot_so_far: u64 =
        conn.recv_payload(&header).map_err(|e| format!("logkeeper's answer is short: {e:?}"))?;
    let [raw] = conn.recv_handles_exact::<1>().ok_or("logkeeper's answer carried no pipe")?;
    // SAFETY: the kernel moved this handle into this table with the frame
    // that names it, and nothing else answers for it.
    let pipe = unsafe { Pipe::from_raw(raw) };
    Ok(Served { pipe, boot_so_far })
}
