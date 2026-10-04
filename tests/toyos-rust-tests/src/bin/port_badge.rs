//! A badge is what the acceptor's holder granted, read back only by that
//! port's acceptor.
//!
//! - A connection made through a minted connector carries its bytes, exactly,
//!   and a connector [`MAX_BADGE`] long is the bound, not past it.
//! - A connection accepted on one port and asked of another port's acceptor is
//!   refused: any process can make a port and mint any bytes on it, so this is
//!   what a forged server end handed to a server meets.
//! - An unbadged connection answers `NotFound`, never an empty badge.
//! - A badge of none or more than [`MAX_BADGE`] bytes is never minted.
//! - A client's end is no accepted connection.
//! - Minting takes `READ` on the acceptor, the right accepting takes.

use toyos::ipc::Connection;
use toyos::namespace;
use toyos::port::{self, Acceptor, Connector};
use toyos::AsHandle;
use toyos_abi::handle::Rights;
use toyos_abi::syscall::{self, SyscallError, MAX_BADGE};

const NAME: &str = "port";

fn main() {
    let (acceptor, _) = port::create().expect("a port");
    let (other, _) = port::create().expect("a second port");
    let mut out = [0u8; MAX_BADGE];

    for badge in [&b"terminal"[..], &[0xa5; MAX_BADGE][..]] {
        let minted = acceptor.mint(badge).expect("mint a badge");
        let (client, server) = connect(&acceptor, &minted);
        assert_eq!(acceptor.badge(&server, &mut out), Ok(badge), "the stamp is not the minted bytes");
        assert_eq!(
            acceptor.badge(&client, &mut out),
            Err(SyscallError::InvalidArgument),
            "a client's end answered a badge"
        );
        assert_eq!(
            other.badge(&server, &mut out),
            Err(SyscallError::PermissionDenied),
            "another port's acceptor read this port's stamp"
        );
    }
    println!("  a minted badge comes back exact on its own port, and on no other");

    let (unbadged_acceptor, unbadged) = port::create().expect("a third port");
    let (_client, server) = connect(&unbadged_acceptor, &unbadged);
    assert_eq!(unbadged_acceptor.badge(&server, &mut out), Err(SyscallError::NotFound));
    println!("  an unbadged connection answers NotFound");

    for len in [0, MAX_BADGE + 1] {
        let badge = vec![1u8; len];
        assert_eq!(
            syscall::port_mint(acceptor.as_handle(), &badge).err(),
            Some(SyscallError::InvalidArgument),
            "a badge of {len} bytes was minted"
        );
    }
    println!("  a badge of 0 or {} bytes is refused", MAX_BADGE + 1);

    let narrowed = syscall::dup_narrowed(acceptor.as_handle(), Rights::DUP.union(Rights::TRANSFER))
        .expect("the acceptor without READ");
    // SAFETY: made by the call above, and owned here alone.
    let narrowed = unsafe { Acceptor::from_raw(narrowed) };
    assert_eq!(narrowed.mint(b"x").err(), Some(SyscallError::PermissionDenied));
    println!("  minting takes READ on the acceptor");

    println!("port_badge: PASS");
}

/// A connection through `connector`, and its end accepted on `acceptor`.
fn connect(acceptor: &Acceptor, connector: &Connector) -> (Connection, Connection) {
    let names = namespace::build().add(NAME, connector).finish().expect("a namespace");
    let client = names.open(NAME).expect("connect");
    let server = acceptor.accept().expect("accept");
    (client, server)
}
