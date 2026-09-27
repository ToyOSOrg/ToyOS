//! An accept netd refuses for room still spends its owner's wake, so the
//! connection it left is announced again once room returns, and not before.
//!
//! The host dials this program's listener through the forward. Woken, this
//! program fills netd's connections to the host until one is refused, and
//! asks for the connection; netd refuses it for room. Once a request netd
//! answered after that refusal says room is still gone, no wake may be
//! waiting. One connection closed, the next wake is the verdict.
//!
//! argv[1] is the port of the harness's host server on `HOST`, and the harness
//! forwards a host port to this guest's `FORWARDED_PORT`.
//! `netd_refused_accept: ok` is the only success line.

#[path = "../netd_stream.rs"]
mod netd_stream;

use netd_stream::{ask, Ask, FORWARDED_PORT, HOST};
use toyos::net::{
    MsgType, NetError, NetdConn, TcpAcceptPipedRequest, TcpAcceptPipedResponse, TcpBound, TcpConnectPipedRequest,
    TcpConnectResponse, TcpConnection, TcpSocketId, DATA_FROM_CLIENT, DATA_HANDLES, DATA_TO_CLIENT,
};
use toyos_abi::syscall::SyscallError;

fn main() {
    let port: u16 = std::env::args()
        .nth(1)
        .and_then(|p| p.parse().ok())
        .expect("usage: netd_refused_accept <host port>");
    let listener = toyos::net::tcp_bind([0; 4], FORWARDED_PORT).expect("bind the forwarded port");

    let dial = toyos::net::tcp_connect(HOST, port, 0).expect("connect to the host server");
    ask(&dial.tx, Ask::Dial);
    wake(&listener, "the host's dial");
    let mut held = Vec::new();
    let refused = loop {
        match connect(port) {
            Ok(conn) => {
                ask(&conn.tx, Ask::Held(0));
                held.push(conn);
            }
            Err(e) => break e,
        }
    };
    assert_eq!(refused, NetError::ResourceExhausted, "a connect after {} held", held.len());
    assert_eq!(
        accept(listener.socket_id),
        Err(NetError::ResourceExhausted),
        "an accept with every connection taken"
    );
    println!("netd_refused_accept: an accept refused for room, {} connections held", held.len());
    assert_eq!(
        connect(port).err(),
        Some(NetError::ResourceExhausted),
        "a connect after an accept refused for room"
    );
    let mut byte = [0u8; 1];
    assert_eq!(
        listener.notify.read_nonblock(&mut byte),
        Err(SyscallError::WouldBlock),
        "netd woke its owner for a connection there is no room to take"
    );
    end(held.pop().expect("the cap holds at least the connection before the refusal"));
    wake(&listener, "the connection an accept refused for room left, once room returned");
    accept(listener.socket_id).unwrap_or_else(|e| panic!("the connection an accept refused for room left: {e:?}"));
    end(dial);
    held.into_iter().for_each(end);
    toyos::net::tcp_close(listener.socket_id).expect("close the listener");
    println!("netd_refused_accept: ok");
}

/// Wait for netd's wake on `listener`, and take it.
fn wake(listener: &TcpBound, what: &str) {
    println!("netd_refused_accept: waiting for a wake for {what}");
    let mut byte = [0u8; 1];
    match listener.notify.read(&mut byte) {
        Ok(1) => {}
        Ok(0) => panic!("a wake for {what}: netd closed the listener"),
        Ok(n) => panic!("a wake for {what}: read {n} bytes"),
        Err(e) => panic!("a wake for {what}: {e:?}"),
    }
}

/// netd's answer to an accept on `listener`. An accepted connection is closed
/// at once.
fn accept(listener: TcpSocketId) -> Result<(), NetError> {
    let (_rx, _tx, handles) = data_path();
    let resp: TcpAcceptPipedResponse = reach_netd()
        .request_with_handles(&handles, MsgType::TcpAcceptPiped, &TcpAcceptPipedRequest { socket_id: listener.0 })
        .expect("netd takes the request")
        .response()?;
    toyos::net::tcp_close(TcpSocketId(resp.socket_id)).expect("close the accepted connection");
    Ok(())
}

/// netd's answer to a connect to the host server.
fn connect(port: u16) -> Result<TcpConnection, NetError> {
    let (rx, tx, handles) = data_path();
    let resp: TcpConnectResponse = reach_netd()
        .request_with_handles(
            &handles,
            MsgType::TcpConnectPiped,
            &TcpConnectPipedRequest { addr: HOST, port, _pad: 0, timeout_ms: 0 },
        )
        .expect("netd takes the request")
        .response()?;
    Ok(TcpConnection { rx, tx, socket_id: TcpSocketId(resp.socket_id), local_port: resp.local_port })
}

/// A connection to netd. **Refused only by the kernel's port queue**, which
/// `toyos::net` spells as netd's own `ResourceExhausted`, so it is no answer
/// of netd's here.
fn reach_netd() -> NetdConn {
    NetdConn::connect().unwrap_or_else(|e| panic!("reach netd: {e:?}"))
}

/// The ends of a duplex data path this side keeps, and the two it hands netd.
fn data_path() -> (toyos::Pipe, toyos::Pipe, [toyos_abi::RawHandle; DATA_HANDLES]) {
    let (rx, to_client) = toyos::pipe_pair().expect("the pipe netd writes into");
    let (from_client, tx) = toyos::pipe_pair().expect("the pipe netd reads from");
    let mut handles = [toyos_abi::HANDLE_INVALID; DATA_HANDLES];
    handles[DATA_TO_CLIENT] = to_client.into_raw();
    handles[DATA_FROM_CLIENT] = from_client.into_raw();
    (rx, tx, handles)
}

fn end(conn: TcpConnection) {
    let TcpConnection { rx, tx, socket_id, .. } = conn;
    drop((rx, tx));
    toyos::net::tcp_close(socket_id).expect("close a connection");
}
