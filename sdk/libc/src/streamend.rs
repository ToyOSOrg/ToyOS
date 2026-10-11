//! How a stream socket's end reaches its caller. A stream is two of netstack's pipes, and the
//! receive pipe says only that it ended: netstack lets the send pipe go first when the connection
//! failed, and keeps it after an orderly end for as long as its writer holds it, so what a
//! zero-byte write into it answers after the end is the end's kind. It reads nothing but what it
//! is handed, so the host tests it (`toyos-libc-copies`).

use toyos_abi::syscall::SyscallError;

/// What a call on a stream answers instead of bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// `ECONNRESET`: the connection failed.
    Reset,
    /// `EAGAIN`.
    Again,
    /// `EIO`: the handle is not the pipe end the socket was made with.
    Other,
}

/// A read of 0 from the receive pipe, given what a zero-byte write into the send pipe then
/// answered: `Ok` is the peer's FIN, after its last byte.
pub(crate) fn read_end(probe: Result<usize, SyscallError>) -> Result<(), Refusal> {
    match probe {
        Ok(_) | Err(SyscallError::WouldBlock) => Ok(()),
        Err(SyscallError::Gone) => Err(Refusal::Reset),
        Err(_) => Err(Refusal::Other),
    }
}

/// A write the send pipe refused. One after a shutdown of the sending half is refused before it
/// reaches the pipe, which netstack reads no more but keeps.
pub(crate) fn write_refused(refused: SyscallError) -> Refusal {
    match refused {
        SyscallError::Gone => Refusal::Reset,
        SyscallError::WouldBlock => Refusal::Again,
        _ => Refusal::Other,
    }
}
