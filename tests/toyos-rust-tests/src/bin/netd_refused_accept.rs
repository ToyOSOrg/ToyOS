//! An accept netd refuses still spends its owner's wake, so the connection it
//! left is announced again: at once after a request that handed netd no
//! pipes, and after a refusal for room once room returns and not before.
//!
//! The host dials this program's listener through the forward, twice:
//!
//! 1. Woken, this program asks for the connection handing netd no pipes, and
//!    netd refuses the request. The next wake is the verdict.
//! 2. Woken, this program fills netd's connections to the host until one is
//!    refused, and asks for the connection; netd refuses it for room. Once a
//!    request netd answered after that refusal says room is still gone, no
//!    wake may be waiting. One connection closed, the next wake is the
//!    verdict.
//!
//! argv[1] is the port of the harness's host server on `HOST`, and the harness
//! forwards a host port to this guest's `FORWARDED_PORT`.
//! `netd_refused_accept: ok` is the only success line.

#[path = "../netd_stream.rs"]
mod netd_stream;

use std::time::Duration;

use netd_stream::{ask, await_until, Ask, FORWARDED_PORT, HOST};
use toyos::net::{
    MsgType, NetError, NetdConn, TcpAcceptPipedRequest, TcpAcceptPipedResponse, TcpBound, TcpConnectPipedRequest,
    TcpConnectResponse, TcpConnection, TcpSocketId, DATA_FROM_CLIENT, DATA_HANDLES, DATA_TO_CLIENT,
};
use toyos::poller::READABLE;
use toyos_abi::syscall::SyscallError;

/// How long netd may take to act on something it has been handed. Orders of
/// magnitude over a pass; a bound, said by name, not a pace.
const WITHIN: Duration = Duration::from_secs(20);

fn main() {
    let port: u16 = std::env::args()
        .nth(1)
        .and_then(|p| p.parse().ok())
        .expect("usage: netd_refused_accept <host port>");
    let listener = toyos::net::tcp_bind([0; 4], FORWARDED_PORT).expect("bind the forwarded port");

    let dial = dial_in(port);
    wake(&listener, "the host's first dial");
    assert_eq!(accept(listener.socket_id, false), Err(NetError::InvalidInput), "an accept handing netd no pipes");
    println!("netd_refused_accept: an accept handing netd no pipes was refused");
    wake(&listener, "the connection an accept handing netd no pipes left");
    accept(listener.socket_id, true).unwrap_or_else(|e| panic!("the connection an accept with no pipes left: {e:?}"));
    end(dial);

    let dial = dial_in(port);
    wake(&listener, "the host's second dial");
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
        accept(listener.socket_id, true),
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
    accept(listener.socket_id, true).unwrap_or_else(|e| panic!("the connection an accept refused for room left: {e:?}"));
    end(dial);
    held.into_iter().for_each(end);
    toyos::net::tcp_close(listener.socket_id).expect("close the listener");
    println!("netd_refused_accept: ok");
}

/// A connection to the host server that asks it to dial this guest's
/// forwarded port.
fn dial_in(port: u16) -> TcpConnection {
    let dial = toyos::net::tcp_connect(HOST, port, 30_000).expect("connect to the host server");
    ask(&dial.tx, Ask::Dial);
    dial
}

/// Wait for netd's wake on `listener`, and take it.
fn wake(listener: &TcpBound, what: &str) {
    let mut byte = [0u8; 1];
    await_until(&listener.notify, READABLE, WITHIN, &format!("a wake for {what}"), || {
        match listener.notify.read_nonblock(&mut byte) {
            Ok(1) => Some(()),
            Ok(_) => panic!("a wake for {what}: netd closed the listener"),
            Err(SyscallError::WouldBlock) => None,
            Err(e) => panic!("a wake for {what}: {e:?}"),
        }
    });
}

/// netd's answer to an accept on `listener`, handing it a pair of pipes or
/// none. An accepted connection is closed at once.
fn accept(listener: TcpSocketId, pipes: bool) -> Result<(), NetError> {
    let request = TcpAcceptPipedRequest { socket_id: listener.0 };
    let pending = if pipes {
        let (_rx, _tx, handles) = data_path();
        reach_netd().request_with_handles(&handles, MsgType::TcpAcceptPiped, &request)
    } else {
        reach_netd().request(MsgType::TcpAcceptPiped, &request)
    };
    let resp: TcpAcceptPipedResponse = pending.expect("netd takes the request").response()?;
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
            &TcpConnectPipedRequest { addr: HOST, port, _pad: 0, timeout_ms: 30_000 },
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
