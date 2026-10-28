//! One request netstack answers: a UDP bind on any address. netstack opens its
//! card before it accepts a connection, so the answer follows the card's
//! bring-up and every line that bring-up logged. Exits 0 on the bind, and 1
//! naming the refusal.

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
