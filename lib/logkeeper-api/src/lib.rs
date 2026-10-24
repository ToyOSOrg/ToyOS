//! Asking `logkeeper` for this boot's log on this machine: one [`READ`] on
//! its [`SERVICE`] port, answered by one [`SERVED`] frame carrying how many
//! bytes are the boot so far and the read end of a pipe the log is written
//! into, from its first line; or the same as the machine's console carrier
//! ([`carry`]).

use toyos::endow;
use toyos::{Connection, Pipe};
use toyos_logstream::{CARRIER, CARRY, DELIVERED, READ, SERVED, SERVICE};

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
    answer(&conn)
}

/// The [`SERVED`] frame a request on `conn` is answered with.
fn answer(conn: &Connection) -> Result<Served, String> {
    let header = conn.recv_header().map_err(|e| format!("logkeeper did not answer: {e:?}"))?;
    if header.msg_type != SERVED {
        return Err(format!("logkeeper answered frame type {}", header.msg_type));
    }
    let boot_so_far: u64 =
        conn.recv_payload(&header).map_err(|e| format!("logkeeper's answer is not one `u64`: {e:?}"))?;
    let [raw] = conn.recv_handles_exact::<1>(&header).ok_or("logkeeper's answer carried no pipe")?;
    // SAFETY: the kernel moved this handle into this table with the frame
    // that names it, and nothing else answers for it.
    let pipe = unsafe { Pipe::from_raw(raw) };
    Ok(Served { pipe, boot_so_far })
}

/// Ask for the log as the machine's console carrier, on
/// [`toyos_logstream::CARRIER`]: the answer is [`read`]'s, and the connection
/// stays open for the carrier to say what its channel has taken
/// ([`delivered`]).
pub fn carry() -> Result<(Connection, Served), String> {
    let conn = endow::service(CARRIER).map_err(|e| format!("no `{CARRIER}` service: {e:?}"))?;
    conn.signal(CARRY).map_err(|e| format!("logkeeper would not take the request: {e:?}"))?;
    let served = answer(&conn)?;
    Ok((conn, served))
}

/// Tell `logkeeper` the carrier's channel has taken `through` bytes of its
/// pipe. Never waits: a later count says everything a refused one would have.
pub fn delivered(conn: &Connection, through: u64) {
    let _ = conn.try_send(DELIVERED, &through);
}
