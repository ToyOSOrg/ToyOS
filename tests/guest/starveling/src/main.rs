//! Test-only: a service on `starved` that answers [`PING`] with [`SERVED`],
//! and on [`STARVE`] asks for memory. Refused it, as on a machine another job
//! has taken, it gives back what it can of its own stack, answers
//! [`RELEASED`] with how much, and ends, exit 3, on the next [`END`]: so
//! what its end frees is less than another start of it needs, whoever took
//! what it gave back. Granted, the premise is gone, and it ends exit 4.

use toyos::endow;
use toyos::ipc::{Connection, FrameRx, RxStep};
use toyos::poller::{Poller, READABLE};
use toyos_abi::syscall::{self, SyscallError, VmAt, VmProt};

const PING: u32 = 1;
const SERVED: u32 = 2;
const STARVE: u32 = 3;
const RELEASED: u32 = 4;
const END: u32 = 5;

/// One commitment of what the test's greedy job left nothing of.
const ASKED: usize = 64 << 20;
const PAGE: usize = 4096;
/// What of the 8 MiB main stack it gives back: everything but the top
/// [`KEPT`] below where it runs, which this program never reaches.
const GIVEN: usize = 6 << 20;
const KEPT: usize = 256 << 10;

const TOKEN_ACCEPT: u64 = 0;
const TOKEN_CLIENT: u64 = 1;

fn main() {
    let acceptor = endow::acceptor("starved").expect("starveling: the manifest declares it serves `starved`");
    let mut client: Option<(Connection, FrameRx<0>)> = None;
    let poller = Poller::new(2);
    println!("starveling: serving");
    loop {
        poller.watch(&acceptor, READABLE, TOKEN_ACCEPT);
        if let Some((conn, _)) = &client {
            poller.watch(conn, READABLE, TOKEN_CLIENT);
        }
        let mut ready = [false; 2];
        poller.wait(1, u64::MAX, |token| ready[token as usize] = true);
        if ready[TOKEN_ACCEPT as usize] {
            if let Ok(conn) = acceptor.accept() {
                client = Some((conn, FrameRx::new()));
            }
        }
        if !ready[TOKEN_CLIENT as usize] {
            continue;
        }
        let Some((conn, rx)) = &mut client else { continue };
        loop {
            match rx.pump(conn) {
                RxStep::Frame { msg_type: PING, .. } => {
                    let _ = conn.try_signal(SERVED);
                }
                RxStep::Frame { msg_type: STARVE, .. } => starve(conn),
                RxStep::Frame { msg_type: END, .. } => {
                    println!("starveling: ending, refused memory");
                    std::process::exit(3);
                }
                RxStep::Frame { .. } | RxStep::Idle => break,
                RxStep::Eof | RxStep::Malformed => {
                    client = None;
                    break;
                }
            }
        }
    }
}

/// Ask for [`ASKED`]; refused, give back [`GIVEN`] of the stack and say so.
fn starve(conn: &Connection) {
    // SAFETY: placed anywhere, so it replaces nothing.
    match unsafe { syscall::vm_reserve(ASKED, VmProt::ReadWrite, VmAt::ANYWHERE) } {
        Err(SyscallError::ResourceExhausted) => {}
        answer => {
            println!("starveling: {ASKED} bytes answered {:?}, which nothing left should have", answer.map(|_| ()));
            std::process::exit(4);
        }
    }
    let here = core::ptr::addr_of!(conn) as usize;
    let top = (here - KEPT) & !(PAGE - 1);
    // SAFETY: the pages below `top` are this thread's own stack, which it
    // never reaches again; a touch there faults as an overflow, not as
    // another range's memory.
    unsafe { syscall::vm_decommit((top - GIVEN) as *mut u8, GIVEN) }.expect("starveling: give back its stack");
    println!("starveling: refused {ASKED} bytes; gave back {GIVEN} of its stack");
    let _ = conn.try_send_bytes(RELEASED, &(GIVEN as u64).to_le_bytes());
}
