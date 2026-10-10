//! libc's rule for a stream's end (`streamend.rs`): a read of 0 is the peer's FIN only while the
//! send pipe still has its reader, a write the send pipe refuses for its reader's leaving is a
//! reset, and nothing else either pipe says is read as one.

use toyos_abi::syscall::SyscallError;

use crate::streamend::{read_end, write_refused, Refusal};

/// Every refusal but the two a pipe answers on purpose.
const OTHERS: [SyscallError; 4] =
    [SyscallError::InvalidArgument, SyscallError::PermissionDenied, SyscallError::BadAddress, SyscallError::Io];

#[test]
fn a_read_of_zero_is_the_peers_fin_only_while_the_send_pipe_is_read() {
    assert_eq!(read_end(Ok(0)), Ok(()));
    assert_eq!(read_end(Err(SyscallError::WouldBlock)), Ok(()), "a full send pipe still has its reader");
    assert_eq!(read_end(Err(SyscallError::Gone)), Err(Refusal::Reset));
    for other in OTHERS {
        assert_eq!(read_end(Err(other)), Err(Refusal::Other), "{other:?}");
    }
}

#[test]
fn a_write_the_send_pipe_refuses_for_its_reader_is_a_reset() {
    assert_eq!(write_refused(SyscallError::Gone), Refusal::Reset);
    assert_eq!(write_refused(SyscallError::WouldBlock), Refusal::Again);
    for other in OTHERS {
        assert_eq!(write_refused(other), Refusal::Other, "{other:?}");
    }
}
