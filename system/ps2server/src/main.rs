//! `/system/bin/ps2server`: the i8042, its keyboard and its pointer, served.
//!
//! The kernel keeps the `isa` claim on the controller's ports and lines and
//! nothing else: it records each interrupt on the claim, and speaks none of
//! the controller's protocol. This program [`probe`]s the controller, then
//! runs the [`stream`], and hands the owner of input each key and pointer
//! report over its connection to the `input` port (`toyos_input::wire`), as
//! one source.
//!
//! **Every start is the same start.** The probe resets the keyboard and the
//! pointer and reads nothing a predecessor or firmware left, so a restart
//! needs no memory of the process before it, and what was typed while none
//! ran is not delivered.
//!
//! **It exits 0 only when it has nothing to serve**: no `isa` claim (a machine
//! with no i8042 row), port 0x64 reading `0xFF`, or the owner of input gone.
//! Every other end is a failure, said by name, which its supervisor restarts.

mod bus;
mod hw;
mod probe;
mod stream;

#[cfg(test)]
mod fake;

use toyos::endow::{self, EndowError};
use toyos::ipc::Connection;
use toyos::Device;
use toyos_abi::syscall::IsaId;
use toyos_input::wire::{self, Record, POINTER_LEN};

use crate::bus::Controller;
use crate::probe::Probed;

/// The i8042's data and command ports, and its keyboard and aux lines.
fn i8042() -> IsaId {
    IsaId::new(&[0x60, 0x64], &[1, 12]).expect("the i8042's set is a set")
}

fn main() {
    let mut say = |line: String| println!("{line}");
    let Some(claim) = endow::isa::<Device>(i8042()) else {
        say("ps2server: no i8042 claim — nothing to serve".to_string());
        std::process::exit(0);
    };
    let mut owner = connect();
    let mut hw = hw::Hw::bind(claim);
    let (aux, backlog) = match probe::probe(&mut hw, &mut say) {
        Probed::Absent => std::process::exit(0),
        Probed::Failed => std::process::exit(1),
        Probed::Armed { aux, backlog } => (aux, backlog),
    };
    let (mut stream, records) = stream::Stream::new(&mut hw, aux, &backlog, &mut say);
    deliver(&mut owner, &records);
    loop {
        let irq = hw.wait(stream.deadline());
        match stream.wake(&mut hw, irq, &mut say) {
            Ok(records) => deliver(&mut owner, &records),
            Err(stream::Wedged) => std::process::exit(1),
        }
    }
}

/// The connection to the owner of input, or the exit an owner that is gone
/// asks for.
fn connect() -> Connection {
    match endow::service(wire::PORT) {
        Ok(conn) => conn,
        Err(EndowError::ServerGone) => {
            println!("ps2server: the owner of input is gone — nothing to serve");
            std::process::exit(0);
        }
        Err(other) => panic!("ps2server: the manifest gives this program `{}`, and it answered {other:?}", wire::PORT),
    }
}

/// Hand the owner `records`, one frame each; an owner that hung up is asked
/// for again, and the new connection is a new source to it.
fn deliver(owner: &mut Connection, records: &[Record]) {
    for record in records {
        let mut payload = [0u8; POINTER_LEN];
        let (msg_type, len) = record.frame(&mut payload);
        if owner.send_bytes(msg_type, &payload[..len]).is_err() {
            *owner = connect();
            println!("ps2server: the owner of input hung up; connected again, and what it held is released");
            return;
        }
    }
}
