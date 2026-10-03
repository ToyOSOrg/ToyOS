//! The `isa` claim's lines: an interrupt the i8042 raises reaches the claim's
//! holder as a record, and one it raises once the claim is gone reaches nobody.
//!
//! Run where the kernel drives no i8042 (`i8042-withheld`), with no key to
//! press: the keyboard is told to enable scanning (`0xF4`) and its
//! acknowledgement (`0xFA`) is the byte that raises the line.
//!
//! - `device` holds the claim: it drives the controller until the
//!   acknowledgement arrives as a record and a byte, gives the claim up, and
//!   has the keyboard acknowledge once more, on a line then masked.
//!   `isa_straddled` runs this role on a controller the kernel's quarantine
//!   let go.
//! - `after` holds the next claim on the row, and finds no record on it.

use std::os::toyos::process::CommandExt;
use std::process::Command;
use std::time::{Duration, Instant};

use toyos::endow::Endowments;
use toyos::poller::{Poller, READABLE};
use toyos::syscap::SysCap;
use toyos::{AsHandle, Device};
use toyos_abi::pci::DeviceIrqRecord;
use toyos_abi::syscall::{self, IsaId, SyscallError, SYSCAP_LABEL};

#[path = "../arch/port.rs"]
mod port;

use port::{port_in, port_out};

const SELF_PATH: &str = "/system/bin/test_rs_isa_lines";
const CLAIM_LABEL: &str = "isa-claim";

/// The i8042's data and command ports, keyboard and aux lines.
const I8042: &str = "0060,0064:1,12";
const DATA: u16 = 0x60;
const STATUS: u16 = 0x64;

const OBF: u8 = 1 << 0;
const IBF: u8 = 1 << 1;

/// The controller's configuration byte: its keyboard interrupt, and the bits
/// that stop the keyboard's clock and the aux port's.
const CFG_KEYBOARD_IRQ: u8 = 1 << 0;
const CFG_KEYBOARD_CLOCK_OFF: u8 = 1 << 4;
const CFG_AUX_CLOCK_OFF: u8 = 1 << 5;

/// The keyboard's enable-scanning command, and its acknowledgement.
const ENABLE_SCANNING: u8 = 0xF4;
const ACK: u8 = 0xFA;

/// A controller answers a command, and a keyboard acknowledges one, in
/// milliseconds; this is the liveness ceiling on one that does not, and it
/// panics by name.
const CONTROLLER: Duration = Duration::from_secs(1);
/// The ceiling on the record behind an acknowledgement.
const RECORD: Duration = Duration::from_secs(5);
/// More records than the holder's own setup can raise before it asks the
/// keyboard anything: one byte answers the configuration read.
const STALE_RECORDS: usize = 8;

fn main() {
    match std::env::args().nth(1).as_deref() {
        None => lines(&syscap()),
        Some("device") => device(taken()),
        Some("after") => after(taken()),
        other => panic!("isa_lines: unknown role {other:?}"),
    }
}

fn syscap() -> SysCap {
    Endowments::get()
        .take(SYSCAP_LABEL)
        .expect("the test estate is endowed a device-minting capability")
}

fn taken() -> Device {
    Endowments::get().take(CLAIM_LABEL).expect("isa: this role is endowed the claim")
}

fn set() -> IsaId {
    IsaId::parse(I8042).expect("the i8042's set")
}

fn claim(cap: &SysCap) -> Result<Device, SyscallError> {
    cap.claim_isa(set())
}

/// Run `role` holding `claim`; it must end cleanly.
fn role(role: &str, claim: Device) {
    let status = Command::new(SELF_PATH)
        .arg(role)
        .endow(CLAIM_LABEL, claim.into_raw().0)
        .status()
        .expect("isa: spawn a role");
    assert!(status.success(), "isa lines: {role} ended {:?}", status.code());
}

fn lines(cap: &SysCap) {
    role("device", claim(cap).expect("isa lines: the i8042 row is free where the kernel drives none"));
    // The row came back with `device`, whose keyboard acknowledged once more
    // after its claim was gone.
    role("after", claim(cap).expect("isa lines: the row came back from the holder before"));
    println!("isa lines: the holder's interrupt reached it, and the one after its claim reached nobody");
}

fn inb(port: u16) -> u8 {
    port_in(port, false) as u8
}

/// Read the claim's description, which binds its ports to this process.
fn bind(claim: &Device) {
    let mut words = [0u8; 16];
    let n = claim.read(&mut words).expect("isa: the claim's first read is its description");
    assert_eq!(n, words.len(), "isa: a description of {n} bytes");
}

/// Spin on the status register until `ready`, or panic naming `what`.
fn wait_status(what: &str, ready: impl Fn(u8) -> bool) {
    let by = Instant::now() + CONTROLLER;
    while !ready(inb(STATUS)) {
        assert!(Instant::now() < by, "isa device: the controller never {what} in {CONTROLLER:?}");
        std::hint::spin_loop();
    }
}

fn command(byte: u8) {
    wait_status("took a command", |s| s & IBF == 0);
    port_out(STATUS, byte);
}

fn data(byte: u8) {
    wait_status("took a data byte", |s| s & IBF == 0);
    port_out(DATA, byte);
}

/// Whatever the output buffer holds, read away.
fn drain() {
    for _ in 0..32 {
        if inb(STATUS) & OBF == 0 {
            return;
        }
        let _ = inb(DATA);
    }
    panic!("isa device: the output buffer never drained");
}

/// Have the keyboard acknowledge, and read the acknowledgement once it is in
/// the output buffer.
fn acknowledged(when: &str) {
    data(ENABLE_SCANNING);
    wait_status("had the keyboard's acknowledgement", |s| s & OBF != 0);
    let byte = inb(DATA);
    assert_eq!(byte, ACK, "isa device: {when}, the keyboard answered {byte:#04x}, not its acknowledgement");
}

/// The record on `claim` if there is one, without waiting.
fn record(claim: &Device) -> Option<u32> {
    let mut record = [0u8; DeviceIrqRecord::SIZE];
    match syscall::read_nonblock(claim.as_handle(), &mut record) {
        Ok(n) => {
            assert_eq!(n, DeviceIrqRecord::SIZE, "isa: a record of {n} bytes");
            Some(u32::from_ne_bytes(record))
        }
        Err(SyscallError::WouldBlock) => None,
        Err(other) => panic!("isa: a bound claim's read answered {other:?}"),
    }
}

/// Arm `poller`'s watch on `claim` with no record behind it. Where the
/// controller came with its keyboard interrupt on, the configuration's own
/// answer raised the line: a record of that is read away and the watch armed
/// again.
fn arm(poller: &Poller, claim: &Device) {
    for _ in 0..STALE_RECORDS {
        let _ = record(claim);
        poller.watch(claim, READABLE, 0);
        let mut ready = false;
        poller.wait(0, 0, |_| ready = true);
        if !ready {
            return;
        }
    }
    panic!("isa device: the claim was readable {STALE_RECORDS} times with nothing asked of the keyboard");
}

/// Drive the controller through `claim` until the keyboard's acknowledgement
/// arrives as a record and a byte, then give the claim up and have it
/// acknowledge again.
fn device(claim: Device) {
    bind(&claim);
    drain();
    // Read the configuration byte, then write it back with the keyboard's line
    // on and its clock running, and the aux port's clock stopped so that every
    // byte from here on is the keyboard's (i8042: 0x20 reads it, 0x60 writes
    // it, 0xAE enables the keyboard's port).
    command(0x20);
    wait_status("answered its configuration", |s| s & OBF != 0);
    let config = inb(DATA);
    command(0x60);
    data((config | CFG_KEYBOARD_IRQ | CFG_AUX_CLOCK_OFF) & !CFG_KEYBOARD_CLOCK_OFF);
    command(0xAE);
    drain();
    println!("isa device: config {config:#04x}, keyboard line on");

    // Armed before the keyboard is asked anything, so the watch is completed
    // by a post from the line's handler and by nothing this thread reads.
    let poller = Poller::new(1);
    arm(&poller, &claim);
    data(ENABLE_SCANNING);
    let mut woke = false;
    poller.wait(1, RECORD.as_nanos() as u64, |_| woke = true);
    assert!(woke, "isa device: no record in {RECORD:?} of the keyboard's acknowledgement");
    let count = record(&claim).expect("isa device: a readable claim reads a record");
    assert!(count >= 1, "isa device: a record counting {count}");
    // The byte is the holder's: nothing else may have read it from under the record.
    wait_status("had the byte behind its interrupt", |s| s & OBF != 0);
    let byte = inb(DATA);
    assert_eq!(byte, ACK, "isa device: the interrupt carried {byte:#04x}, not the acknowledgement");
    println!("isa device: {count} interrupt(s), the keyboard's acknowledgement behind them");

    // The claim goes and the ports stay this process's: the keyboard's next
    // acknowledgement raises a line no claim holds.
    drop(claim);
    acknowledged("with its claim gone");
    // The configuration the controller came with.
    command(0x60);
    data(config);
    println!("isa device: the keyboard acknowledged once more after the claim went");
}

/// The claim after `device`'s: nothing raised the line under it, so it holds
/// no record.
fn after(claim: Device) {
    bind(&claim);
    if let Some(count) = record(&claim) {
        panic!(
            "isa after: a claim nothing raised a line under carries a record of {count}: the line \
             was open while no claim held it"
        );
    }
    println!("isa after: no record on a claim nothing raised a line under");
}
