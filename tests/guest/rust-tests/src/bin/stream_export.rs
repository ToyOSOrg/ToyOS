//! A stream a real netstack holds, exported to a child through std's
//! `Command` and adopted there: the one path from a connect to a second
//! process's write that only a guest with netstack shows.
//!
//! The harness listens on its host's loopback, which QEMU's user network
//! reaches from the guest as `10.0.2.2`, and names its port as this job's
//! argument. The parent connects there, spawns a child given the stream as
//! descriptor 3 (`CommandExt::inherit_handle`, which exports it), and waits
//! for its end; the child takes the descriptor up as its holder and writes a
//! line. The parent then writes its own, and both lines reach the harness on
//! one connection, in that order, before its FIN.

use std::io::Write;
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::os::fd::FromRawFd;
use std::os::toyos::process::CommandExt;
use std::process::{Command, Stdio};

const SELF_PATH: &str = "/system/bin/test_rs_stream_export";

/// QEMU's user network's address of its host.
const HOST: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 2);

/// The descriptor the child is given its holder of the stream under.
const HELD_FD: i32 = 3;

/// What the child writes, then the parent.
const CHILD_LINE: &[u8] = b"adopted by the child\n";
const PARENT_LINE: &[u8] = b"still the parent's\n";

fn main() {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("child") => child(),
        Some(port) => parent(port.parse().unwrap_or_else(|e| panic!("a port, not {port:?}: {e}"))),
        None => panic!("the harness's port is this job's argument"),
    }
}

fn parent(port: u16) {
    let mut stream = TcpStream::connect(SocketAddr::from((HOST, port))).expect("a connect through netstack to the harness");
    let status = Command::new(SELF_PATH)
        .arg("child")
        .inherit_handle(HELD_FD as u32, std::os::toyos::io::AsHandle::as_handle(&stream))
        .stdout(Stdio::null())
        .status()
        .expect("spawn the child with a holder of the stream");
    assert!(status.success(), "the child ended {status:?}");
    stream.write_all(PARENT_LINE).expect("the parent's write after the child's");
    println!("a stream exported to a child: the child wrote on it, and so did the parent");
}

fn child() {
    // SAFETY: the descriptor its parent gave it, a holder of the stream.
    let mut stream = unsafe { TcpStream::from_raw_fd(HELD_FD) };
    stream.write_all(CHILD_LINE).expect("the child's write");
}
