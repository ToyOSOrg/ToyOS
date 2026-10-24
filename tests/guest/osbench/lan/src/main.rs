//! `osbench-peer`: the far end of a bench's `osbench lan`, the fixed reference
//! on the other bench's Linux. It answers discovery and serves requests on
//! [`osbench_lan::PORT`] until it is stopped, and names no address.

use std::net::{Ipv4Addr, TcpListener, UdpSocket};

fn main() {
    let port = osbench_lan::PORT;
    let udp = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, port)).unwrap_or_else(|e| fail(&format!("UDP port {port}: {e}")));
    let tcp = TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).unwrap_or_else(|e| fail(&format!("TCP port {port}: {e}")));
    std::thread::spawn(move || {
        if let Err(e) = osbench_lan::answer(&udp) {
            fail(&format!("discovery: {e}"));
        }
    });
    println!("osbench-peer: answering on UDP and TCP port {port}; stop with Ctrl-C");
    if let Err(e) = osbench_lan::serve(&tcp, |said| println!("osbench-peer: {said}")) {
        fail(&format!("requests: {e}"));
    }
}

fn fail(why: &str) -> ! {
    eprintln!("osbench-peer: {why}");
    std::process::exit(1);
}
