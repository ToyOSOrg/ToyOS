//! A client's side of a disk's description and of a designation, asked of
//! the block service through a tables or a designation badge
//! (`toyos_blockring::wire::Badge`).

use toyos::namespace::Namespace;
use toyos_blockring::wire::{self, Designated, DiskHead, Refusal, SlotLine};

use crate::session::{answer, connect};
use crate::Error;

/// The longest frame either answer carries.
const FRAME: usize = 4 + 2 * (2 + wire::MAX_STATE) + 64;

/// One frame's type and payload on `conn`, within the answer bound.
fn frame(conn: &toyos::ipc::Connection) -> Result<(u32, Vec<u8>), Error> {
    let mut rx = toyos::ipc::FrameRx::<FRAME>::new();
    let frame = answer(conn, &mut rx)?;
    Ok((frame.msg_type, rx.payload(frame.payload_len).to_vec()))
}

fn refused(payload: &[u8]) -> Error {
    Refusal::decode(payload).map_or(Error::Protocol, Error::Refused)
}

/// What disk `disk` (the service's number, from a listing) holds: its head
/// and every used entry of its table in force.
pub fn describe(names: &Namespace, service: &str, disk: u32) -> Result<(DiskHead, Vec<SlotLine>), Error> {
    let conn = connect(names, service)?;
    conn.send_bytes(wire::MSG_DESCRIBE, &disk.to_le_bytes()).map_err(|_| Error::Ended)?;
    let head = match frame(&conn)? {
        (wire::MSG_DISK, payload) => DiskHead::decode(&payload).ok_or(Error::Protocol)?,
        (wire::MSG_REFUSED, payload) => return Err(refused(&payload)),
        _ => return Err(Error::Protocol),
    };
    let mut slots = Vec::new();
    loop {
        match frame(&conn)? {
            (wire::MSG_SLOT, payload) => slots.push(SlotLine::decode(&payload).ok_or(Error::Protocol)?),
            (wire::MSG_DESCRIBED, payload) if payload.is_empty() => return Ok((head, slots)),
            _ => return Err(Error::Protocol),
        }
    }
}

/// Carry out the designation the connector `service` names was minted for:
/// write the stamp when `commit`, judge only otherwise.
pub fn designate(names: &Namespace, service: &str, commit: bool) -> Result<Designated, Error> {
    let conn = connect(names, service)?;
    conn.send_bytes(wire::MSG_DESIGNATE, &[u8::from(commit)]).map_err(|_| Error::Ended)?;
    match frame(&conn)? {
        (wire::MSG_DESIGNATED, payload) => Designated::decode(&payload).ok_or(Error::Protocol),
        (wire::MSG_REFUSED, payload) => Err(refused(&payload)),
        _ => Err(Error::Protocol),
    }
}
