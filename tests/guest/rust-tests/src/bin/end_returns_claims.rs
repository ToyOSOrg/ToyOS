//! A holder's end gives its claim back before anyone can see it end: the
//! function `xhci-leave=` leaves is handed to a child that holds it, the
//! child is killed and waited for, and the claim is asked for at once, for
//! [`ROUNDS`] rounds. Any refusal is red; how long each step took is printed
//! and not judged.
//!
//! - no argument: the rounds.
//! - `hold`: the child, which holds the claim it was endowed until it is killed.

#[path = "../function.rs"]
mod function;

use std::io::{BufRead, BufReader};
use std::os::toyos::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::syscap::SysCap;
use toyos::PciDev;
use toyos_abi::syscall::{PciId, SyscallError};

const SELF_PATH: &str = "/system/bin/test_rs_end_returns_claims";

/// The controllers a boot of this job leaves to a claim: QEMU's `qemu-xhci`
/// and the T14's Type-C controller, as `usbd_spare`'s.
const SPARES: [PciId; 3] = [
    PciId { vendor: 0x1b36, device: 0x000d },
    PciId { vendor: 0x8086, device: 0x9a13 },
    PciId { vendor: 0x1022, device: 0x1639 },
];

const ROUNDS: u32 = 100;

/// The label the holder is endowed its claim under.
const CLAIM_LABEL: &str = "claim";

/// What the child says once it holds its claim.
const HELD: &str = "held";

fn main() {
    match std::env::args().nth(1).as_deref() {
        None => rounds(),
        Some("hold") => hold(),
        Some(other) => panic!("end_returns_claims: no arm {other:?}"),
    }
}

fn spelled(id: PciId) -> String {
    format!("pci:{:04x}:{:04x}", id.vendor, id.device)
}

/// The one spare this machine has, claimed.
fn claim(cap: &SysCap) -> (PciId, PciDev) {
    let mut found = None;
    for id in SPARES {
        match function::claim::<PciDev>(&cap, id) {
            Err(SyscallError::NotFound) => {}
            Ok(dev) => {
                assert!(found.is_none(), "end_returns_claims: two spares on one machine");
                found = Some((id, dev));
            }
            Err(why) => panic!("end_returns_claims: the claim on {} was refused: {why:?}", spelled(id)),
        }
    }
    found.expect("end_returns_claims: neither spare controller is on this machine")
}

fn rounds() {
    let cap: SysCap = Endowments::get().take(SYSCAP_LABEL).expect("test-runner endows a device-minting capability");
    let (id, mut dev) = claim(&cap);
    let (mut ended, mut answered) = (Duration::ZERO, Duration::ZERO);
    for round in 0..ROUNDS {
        let mut child = Command::new(SELF_PATH)
            .arg("hold")
            .endow(CLAIM_LABEL, toyos::AsHandle::as_handle(&dev).0)
            .stdout(Stdio::piped())
            .spawn()
            .expect("end_returns_claims: spawn the holder");
        // Moved: the spawn holds it now.
        std::mem::forget(dev);
        let mut said = String::new();
        BufReader::new(child.stdout.take().expect("the holder's piped stdout"))
            .read_line(&mut said)
            .expect("end_returns_claims: the holder's line");
        assert_eq!(said.trim_end(), HELD, "end_returns_claims: round {round}'s holder said {said:?}");

        let killed = Instant::now();
        child.kill().expect("end_returns_claims: kill the holder");
        let status = child.wait().expect("end_returns_claims: wait for the holder");
        assert!(!status.success(), "end_returns_claims: a killed holder ended {status:?}");
        let waited = Instant::now();
        dev = function::claim::<PciDev>(&cap, id).unwrap_or_else(|why| {
            panic!("end_returns_claims: round {round}: the claim after its holder's end was refused: {why:?}")
        });
        ended += waited - killed;
        answered += waited.elapsed();
    }
    println!(
        "end_returns_claims: {ROUNDS} rounds, every claim granted after its holder's end; mean kill to end {} µs, \
         end to claim {} µs",
        (ended / ROUNDS).as_micros(),
        (answered / ROUNDS).as_micros()
    );
}

fn hold() {
    let dev: PciDev = Endowments::get()
        .take(CLAIM_LABEL)
        .expect("end_returns_claims: the holder is endowed the claim");
    dev.describe().expect("end_returns_claims: the held claim's description");
    println!("{HELD}");
    loop {
        std::thread::park();
    }
}
