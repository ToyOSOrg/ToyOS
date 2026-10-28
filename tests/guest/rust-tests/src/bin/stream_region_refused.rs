//! A connect whose region has none of its pages committed does not fault
//! netstack, which goes on serving: the header and both rings' first sizes
//! are made memory where netstack judges the region, charged to the client,
//! before anything reads them, or the region is refused there.
//!
//! It needs the guest's real netstack: the judge is netstack's own, and the
//! fault it prevents would end netstack, which only the kernel delivers.
//! `netstack_holders_exported_to_children`'s boot runs it, on
//! tests/images/netcase.toml, whose netstack has its lease.

use std::net::{Ipv4Addr, SocketAddrV4, TcpListener};

use toyos::net::stream::REGION_BYTES;
use toyos::net::{MsgType, NetstackConn, TcpConnectRequest, TcpConnectResponse};
use toyos::OwnedHandle;
use toyos_abi::syscall;

fn main() {
    let region = syscall::shm_reserve(REGION_BYTES, 0).expect("a region with nothing committed");
    // SAFETY: a handle the kernel just made, which nothing else answers for.
    let region = unsafe { OwnedHandle::from_raw(region) };
    let request = TcpConnectRequest { addr: [192, 0, 2, 9], port: 9, _pad: 0, timeout_ms: 1_000 };
    let answer = NetstackConn::connect()
        .and_then(|conn| conn.request_with_handles([region], MsgType::TcpConnect, &request))
        .and_then(|pending| pending.answer_with_handles::<TcpConnectResponse, 2>());
    match answer {
        Err(refused) => println!("  PASS: a connect with no page of its region committed was refused: {refused:?}"),
        Ok(_) => println!("  PASS: a connect with no page of its region committed had its first pages made memory, and was answered"),
    }
    let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)).expect("netstack serves on after the connect");
    drop(listener);
    println!("  PASS: netstack serves on");
    println!("stream_region_refused: a client's uncommitted region never faults netstack");
}
