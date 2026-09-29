//! netd's piped-connection cap, from the client side.
//!
//! Needs netd with a NIC in front of it and the harness's host server behind
//! it, which only `tests/netcase` provides — it is in `RUST_SKIP` and
//! `netd_connection_caps` runs it there.
//!
//! Every connect goes to the host server and is answered before the next is
//! asked, and every one it grants is held open: the cap counts established
//! connections, so the first `ResourceExhausted` is the boundary, and no clock
//! decides where it falls. Where the boundary falls is measured here and
//! compared with the cap netd announced by the host.

#[path = "../netd_stream.rs"]
mod netd_stream;

use netd_stream::{HOST, NO_DEADLINE};
use toyos::net::{
    MsgType, NetError, NetdConn, TcpConnectPipedRequest, TcpConnectResponse, DATA_FROM_CLIENT,
    DATA_HANDLES, DATA_TO_CLIENT,
};
use toyos::Pipe;
use toyos_abi::syscall;

/// How far past the boundary to keep asking. Small: the point is to cross the
/// boundary, and every request costs netd an IPC connection.
const MARGIN: usize = 4;

/// One netd event-loop pass, which is what a connect the kernel's queue
/// refused waits for before it asks again. A pace and never a verdict.
const PASS_NANOS: u64 = 1_000_000;

fn main() {
    let port: u16 = std::env::args()
        .nth(1)
        .and_then(|p| p.parse().ok())
        .expect("usage: netd_caps <host port>");
    let request = TcpConnectPipedRequest { addr: HOST, port, _pad: 0, timeout_ms: NO_DEADLINE };

    let mut held: Vec<[Pipe; DATA_HANDLES]> = Vec::new();
    let granted = loop {
        match connect(&request) {
            Ok(kept) => held.push(kept),
            Err(NetError::ResourceExhausted) => break held.len(),
            Err(e) => panic!("connect {}: {e:?}, not a capacity refusal", held.len()),
        }
    };
    // Both sides of the boundary, because "a refusal happened" is also true of
    // a netd that refused everything, and of one that refused at random.
    for past in 1..=MARGIN {
        assert_eq!(
            connect(&request).err(),
            Some(NetError::ResourceExhausted),
            "connect {} past the boundary at {granted} was not a capacity refusal",
            granted + past,
        );
    }
    assert!(granted >= 2, "only {granted} connects were accepted; netd is refusing, not bounding");
    println!("netd caps: {granted} connections accepted then refused");
    drop(held);
}

/// One connect, answered before this returns. What a granted one answers is
/// this side's two ends of its data path: while they are held netd holds the
/// connection, which is exactly where the cap is counting it.
fn connect(request: &TcpConnectPipedRequest) -> Result<[Pipe; DATA_HANDLES], NetError> {
    let (to_client_read, to_client_write) = toyos::pipe_pair().expect("the pipe netd writes into");
    let (from_client_read, from_client_write) =
        toyos::pipe_pair().expect("the pipe netd reads from");
    let mut handles = [toyos_abi::HANDLE_INVALID; DATA_HANDLES];
    handles[DATA_TO_CLIENT] = to_client_write.into_raw();
    handles[DATA_FROM_CLIENT] = from_client_read.into_raw();
    netd()
        .request_with_handles(&handles, MsgType::TcpConnectPiped, request)
        .unwrap_or_else(|e| panic!("netd would not take a connect: {e:?}"))
        .response::<TcpConnectResponse>()?;
    Ok([to_client_read, from_client_write])
}

/// A connection to netd, asking again with no bound while the kernel's queue
/// of connections netd has not accepted yet is full.
///
/// That refusal is backpressure from the kernel, retryable against the same
/// peer; netd's own cap is the `ResourceExhausted` in a *response*, which is
/// what this file is about and what it must not be confused with.
fn netd() -> NetdConn {
    loop {
        match NetdConn::connect() {
            Ok(conn) => return conn,
            Err(NetError::ResourceExhausted) => syscall::nanosleep(PASS_NANOS),
            Err(e) => panic!("could not reach netd: {e:?}"),
        }
    }
}
