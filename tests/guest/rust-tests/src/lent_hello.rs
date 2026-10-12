//! A raw client's hello to `fs:/home`, lending a window of the caller's
//! choosing, as the lent-window tests make it.

use toyos::fs::{Reply, Request, HELLO, REPLY, WINDOW_BYTES};
use toyos::ipc::IpcError;
use toyos::shm::SharedMemory;
use toyos::OwnedHandle;
use toyos_abi::syscall::SyscallError;

/// What `fs:/home` answers a hello lending `window`: the volume, which is the
/// server's pid, or `Gone` for a server that let the client go unanswered.
pub fn hello(window: OwnedHandle) -> Result<u64, SyscallError> {
    let names = toyos::endow::namespace().expect("a namespace");
    let conn = names.open("fs:/home").expect("fs:/home");
    conn.send_handles([window]).expect("the window into the connection");
    conn.send(HELLO, &Request::new()).expect("the hello");
    let header = match conn.recv_header() {
        Ok(header) => header,
        Err(IpcError::Disconnected | IpcError::Syscall(SyscallError::Gone)) => return Err(SyscallError::Gone),
        Err(e) => panic!("the hello's answer: {e:?}"),
    };
    assert_eq!(header.msg_type, REPLY, "a hello answered by a frame that is no reply");
    let reply: Reply = conn.recv_payload(&header).expect("the reply's words");
    assert_eq!(reply.status, 0, "a hello refused with a word, not let go");
    Ok(reply.value2)
}

/// A region of `bytes`, for lending.
pub fn region(bytes: usize) -> OwnedHandle {
    SharedMemory::create(bytes).expect("a region").share().expect("its handle")
}

/// A window the server serves.
pub fn window() -> OwnedHandle {
    region(WINDOW_BYTES)
}
