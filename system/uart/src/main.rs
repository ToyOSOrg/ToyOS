//! `/system/bin/uart` — the console UART's driver: the one writer of the UART
//! from the kernel's hand-over before userland runs to the machine's stop.
//!
//! **It carries the log as `logkeeper` hands it over** on
//! [`toyos_logstream::WIRE`], from the first line the kernel did not put on
//! the UART itself, and says how much of it the UART has taken
//! ([`logkeeper_api::delivered`]), which is what the kernel's stop and fatal
//! paths start after. While a VM's console carries the log, `logkeeper`
//! hands it nothing but the line that says so.
//!
//! **What the UART receives is the console's input**: written into the pipe
//! the supervisor hands this program ([`toyos_logstream::CONSOLE_INPUT`]),
//! whose read end is every program's standard input. A byte no reader has
//! made room for is dropped, as a UART nobody reads drops it, and said.
//!
//! The kernel programmed the line before userland ran; this program moves
//! bytes and asks for interrupts ([`line`]), and waits on nothing else. It
//! exits 0 only on a machine with no console UART.

mod hw;
mod line;

use toyos::poller::{Poller, READABLE};
use toyos::{endow, say, Pipe, UartDev};
use toyos_abi::syscall::{DeviceType, SyscallError};
use toyos_logstream::{CONSOLE_INPUT, WIRE};

use hw::Machine;
use line::Line;

const TOKEN_CLAIM: u64 = 0;
const TOKEN_LOG: u64 = 1;

/// What one read of the log takes: what a 16550 at 38400 baud sends in about
/// a second, and a round of a busy boot's lines.
const READ: usize = 4096;

fn main() {
    let Some(claim) = endow::device::<UartDev>(DeviceType::Uart) else {
        say!("uart: no console UART claim — nothing to drive");
        std::process::exit(0);
    };
    let (info, frame) = claim.open().unwrap_or_else(|e| panic!("uart: the claim's description: {e:?}"));
    let level = info.level != 0;
    let mut uart = Machine::open(&info, frame).unwrap_or_else(|| panic!("uart: a UART this program does not know: {info:?}"));
    let input: Option<Pipe> = endow::Endowments::get().take(CONSOLE_INPUT);
    let mut log = match logkeeper_api::carry(WIRE) {
        Ok((conn, served)) => {
            say!("uart: carrying the log on the console UART, from where the kernel left it");
            Some((conn, served.pipe))
        }
        Err(why) => {
            say!("uart: carrying no log: {why}");
            None
        }
    };

    let poller = Poller::new(2);
    let mut line = Line::default();
    let mut told = 0u64;
    let mut received = Vec::new();
    // Bytes dropped since the input last took every byte received.
    let mut dropped = 0u64;
    let mut buf = vec![0u8; READ];
    loop {
        let standing = line.serve(&mut uart, &mut received);
        if let Some(input) = &input
            && !received.is_empty()
        {
            match hand_on(input, &received) {
                0 if dropped > 0 => {
                    say!("uart: the console's input takes bytes again, after {dropped} were dropped");
                    dropped = 0;
                }
                0 => {}
                lost => {
                    if dropped == 0 {
                        say!("uart: the console's input is full; what the UART receives is dropped until it is read");
                    }
                    dropped += lost;
                }
            }
        }
        received.clear();
        if let Some((conn, pipe)) = &log {
            if line.sent() > told {
                told = line.sent();
                logkeeper_api::delivered(conn, told);
            }
            if !line.owes() {
                match pipe.read_nonblock(&mut buf) {
                    Ok(0) => {
                        say!("uart: logkeeper closed the log's pipe; the UART carries nothing more of it");
                        log = None;
                    }
                    Ok(n) => {
                        line.owe(&buf[..n]);
                        continue;
                    }
                    Err(SyscallError::WouldBlock) => {}
                    Err(e) => panic!("uart: logkeeper's pipe refused a read: {e:?}"),
                }
            }
        }
        if standing {
            continue;
        }
        if level {
            claim.ack().unwrap_or_else(|e| panic!("uart: the claim refused its acknowledgement: {e:?}"));
        }
        poller.watch(&claim, READABLE, TOKEN_CLAIM);
        if let Some((_, pipe)) = &log
            && !line.owes()
        {
            poller.watch(pipe, READABLE, TOKEN_LOG);
        }
        let mut interrupted = false;
        poller.wait(1, u64::MAX, |token| interrupted |= token == TOKEN_CLAIM);
        if interrupted {
            // The records are counts; serving the UART is what answers them.
            while claim.irq().is_ok() {}
        }
    }
}

/// `received` into the console's input, as much as its readers have room
/// for: answers how many bytes found none.
fn hand_on(input: &Pipe, received: &[u8]) -> u64 {
    let mut at = 0;
    while at < received.len() {
        match input.write_nonblock(&received[at..]) {
            Ok(n) => at += n,
            Err(SyscallError::WouldBlock) => break,
            Err(e) => panic!("uart: the console's input refused a write: {e:?}"),
        }
    }
    (received.len() - at) as u64
}
