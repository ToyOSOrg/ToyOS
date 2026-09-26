//! A UDP datagram the client's receive pipe will not take whole ends that
//! socket by name, and no other.
//!
//! netd answers a receive with the datagram's length once the bytes are in the
//! client's pipe, and a pipe write takes what room there is: a pipe that took
//! part of one would splice the next datagram onto it. So a partial write
//! ends the socket, and the client asking for the datagram is refused.
//!
//! The full socket is this program's own making: it keeps a second handle to
//! the write end it hands netd, fills the pipe through it, and reads back
//! [`ROOM`] bytes, so netd's write of a [`DATAGRAM`]-byte datagram takes
//! exactly `ROOM`. A second, ordinary socket must then still get its datagram.
//!
//! **The socket that ended is gone whole**: its port binds again, and the pipe
//! netd wrote into reads end-of-file once this program has let go of its own
//! write end, because netd has let go of the one it was handed.
//!
//! argv[1] is the port of the harness's host server, which this program does
//! not use; argv[2] is the port of the harness's UDP echo on `HOST`.
//! `netd_udp_refused: ok` is the only success line.

#[path = "../netd_stream.rs"]
mod netd_stream;

use std::sync::mpsc;
use std::time::Duration;

use netd_stream::{fill, HOST};
use toyos::net::{
    udp_bind, udp_recv_from, udp_send_to, MsgType, NetError, NetdConn, UdpBindRequest,
    UdpBindResponse, UdpRecvResponse, UdpSocketId,
};
use toyos::{AsHandle, Pipe};
use toyos_abi::syscall::{self, SyscallError};

/// Both sockets bind every address, as an ordinary client's does.
const ANY: [u8; 4] = [0, 0, 0, 0];

/// Bytes of room left in the full socket's pipe.
const ROOM: usize = 100;

/// Bytes in each datagram: more than [`ROOM`], less than one Ethernet frame.
const DATAGRAM: usize = 1000;

/// How long netd may take to answer a receive. A bound, said by name; not a
/// pace.
const WITHIN: Duration = Duration::from_secs(20);

fn main() {
    let echo: u16 = std::env::args()
        .nth(2)
        .and_then(|p| p.parse().ok())
        .expect("usage: netd_udp_refused <host port> <echo port>");

    let healthy = udp_bind(ANY, 0).expect("bind an ordinary socket");

    let (rx, kept) = toyos::pipe_pair().expect("a receive pipe");
    let handed = syscall::dup(kept.as_handle()).expect("a second handle to the receive pipe's write end");
    let capacity = fill(&kept);
    // netd's handle is the pipe's only writer from here on.
    drop(kept);
    let mut room = [0u8; ROOM];
    assert_eq!(rx.read_nonblock(&mut room), Ok(ROOM), "making room in the full pipe");
    let (from_client, tx) = toyos::pipe_pair().expect("a send pipe");
    let full: UdpBindResponse = NetdConn::connect()
        .expect("netd is serving")
        .request_with_handles(
            &[handed, from_client.into_raw()],
            MsgType::UdpBind,
            &UdpBindRequest { addr: ANY, port: 0, _pad: 0 },
        )
        .expect("netd takes the request")
        .response()
        .expect("netd binds");
    let full_id = UdpSocketId(full.socket_id);
    println!("netd_udp_refused: socket {} has {ROOM} bytes of room in a {capacity}-byte pipe", full.socket_id);

    send(full_id, &tx, echo, 0xA5);
    match recv(full_id) {
        Err(NetError::ConnectionReset) => {}
        Ok(r) => panic!("a {DATAGRAM}-byte datagram into {ROOM} bytes of room was answered {} bytes", r.len),
        Err(e) => panic!("a {DATAGRAM}-byte datagram into {ROOM} bytes of room was refused {e:?}, not by a reset"),
    }
    assert_eq!(
        recv(full_id).err(),
        Some(NetError::NotConnected),
        "the socket that could not take a datagram whole is still there",
    );
    println!("netd_udp_refused: the socket whose pipe would not take a datagram whole is gone");

    let again = udp_bind(ANY, full.bound_port)
        .unwrap_or_else(|e| panic!("port {} of the ended socket would not bind again: {e:?}", full.bound_port));
    assert_eq!(again.bound_port, full.bound_port);
    let mut drained = 0usize;
    let mut chunk = vec![0u8; 65536];
    loop {
        match rx.read_nonblock(&mut chunk) {
            Ok(0) => break,
            Ok(n) => drained += n,
            Err(SyscallError::WouldBlock) => {
                panic!("netd still holds the ended socket's receive pipe, {drained} bytes read out of it")
            }
            Err(e) => panic!("reading the ended socket's receive pipe: {e:?}"),
        }
    }
    assert_eq!(drained, capacity as usize, "the pipe held its fill and the {ROOM} bytes netd wrote");
    println!("netd_udp_refused: its port binds again and its pipe has no writer left");

    send(healthy.socket_id, &healthy.tx, echo, 0x5A);
    let answer = recv(healthy.socket_id).expect("the ordinary socket's datagram");
    assert_eq!(answer.len as usize, DATAGRAM, "the ordinary socket's datagram length");
    let mut got = vec![0u8; DATAGRAM];
    let n = healthy.rx.read_nonblock(&mut got).expect("the answered datagram is in the pipe");
    assert_eq!(n, DATAGRAM, "the ordinary socket's pipe holds the datagram it was answered");
    assert!(got.iter().all(|&b| b == 0x5A), "the ordinary socket's datagram came back changed");
    println!("netd_udp_refused: ok");
}

/// Send one [`DATAGRAM`] of `byte` from `socket` to the host's echo.
fn send(socket: UdpSocketId, tx: &Pipe, echo: u16, byte: u8) {
    let datagram = [byte; DATAGRAM];
    assert_eq!(tx.write(&datagram), Ok(DATAGRAM), "writing the datagram for netd");
    let sent = udp_send_to(socket, HOST, echo, DATAGRAM as u16).expect("netd sends the datagram");
    assert_eq!(sent as usize, DATAGRAM, "netd sent part of the datagram");
}

/// Ask netd for `socket`'s next datagram, and panic by name if no answer
/// comes within [`WITHIN`].
fn recv(socket: UdpSocketId) -> Result<UdpRecvResponse, NetError> {
    let (answered, answer) = mpsc::channel();
    std::thread::spawn(move || answered.send(udp_recv_from(socket, DATAGRAM as u32)));
    answer
        .recv_timeout(WITHIN)
        .unwrap_or_else(|_| panic!("netd did not answer a receive on socket {} within {WITHIN:?}", socket.0))
}
