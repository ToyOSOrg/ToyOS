//! One request netstack answers: a UDP bind on any address. netstack serves
//! once a NIC driver's link is open, which the driver opens after its card's
//! bring-up, so the answer follows every line that bring-up logged. Exits 0
//! on the bind, and 1 naming the refusal.

use std::net::{Ipv4Addr, UdpSocket};
use std::process::ExitCode;

fn main() -> ExitCode {
    match UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)) {
        Ok(socket) => {
            println!("netstack_answers: bound {:?}", socket.local_addr());
            ExitCode::SUCCESS
        }
        Err(e) => {
            println!("netstack_answers: the bind was refused: {e}");
            ExitCode::FAILURE
        }
    }
}
