//! `host <name>...`: each name's IPv4 addresses, one line apiece, asked the
//! way every Rust program asks, through `std::net::ToSocketAddrs`, which
//! reaches netd's resolver.
//!
//! Named for the BIND utility whose line it prints, `<name> has address <a>`:
//! the resolver hands a client addresses and nothing else, which is what that
//! tool answers. `dig`'s view of the whole message and `nslookup`'s
//! interactive mode would have nothing under them here.
//!
//! Exits 0 when every name had an address, and 1 otherwise, each failure
//! said on stderr as `host: <name>: <kind>: <why>`.

use std::net::{SocketAddr, ToSocketAddrs};
use std::process::ExitCode;

fn main() -> ExitCode {
    let names: Vec<String> = std::env::args().skip(1).collect();
    if names.is_empty() {
        eprintln!("usage: host <name>...");
        return ExitCode::from(2);
    }
    let mut failed = false;
    for name in &names {
        match (name.as_str(), 0).to_socket_addrs() {
            Ok(addrs) => {
                let mut none = true;
                for addr in addrs {
                    none = false;
                    match addr {
                        SocketAddr::V4(a) => println!("{name} has address {}", a.ip()),
                        SocketAddr::V6(a) => println!("{name} has IPv6 address {}", a.ip()),
                    }
                }
                if none {
                    eprintln!("host: {name}: the lookup answered with no address");
                    failed = true;
                }
            }
            Err(e) => {
                eprintln!("host: {name}: {}: {e}", e.kind());
                failed = true;
            }
        }
    }
    if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS }
}
