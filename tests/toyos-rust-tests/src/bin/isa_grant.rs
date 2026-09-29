//! The `isa` claim: the i8042's two ports opened in the I/O permission bitmap
//! for the one process that bound them, and its lines answered as records.
//!
//! Roles, one per machine the harness boots:
//!
//! - `driven`: the kernel drives the i8042 here, and the claim is refused.
//! - `grant`: a machine with no i8042 (`i8042=off`), where the ports float and
//!   nothing the kernel drives stands in the way. Every port access that must
//!   be refused is made in a child of its own, which prints a marker, makes the
//!   one access and must never print again; the kernel's record of each kill
//!   is the harness's to read.
//! - `device`: the real controller the kernel gave up on at boot, driven from
//!   here until a keystroke arrives as a record and a byte.
//! - `straddled`: `device`, on a controller the kernel drove until its
//!   quarantine, the claim asked for until the kernel lets it go.
//! - `released`: nothing, run after `device` has ended, so the key the host
//!   presses in between has had its interrupt.
//!
//! The children: `unbound` holds the claim and never read it, `bound` read it
//! and steps one port past what it was granted, `wide` read it and makes a
//! two-byte access at its data port, `unclaimed` holds nothing, and `moved` was
//! handed a claim its parent had already bound.

use std::os::toyos::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use toyos::endow::Endowments;
use toyos::poller::{Poller, READABLE};
use toyos::syscap::SysCap;
use toyos::{AsHandle, Device};
use toyos_abi::pci::DeviceIrqRecord;
use toyos_abi::syscall::{self, IsaId, SyscallError, SYSCAP_LABEL};

const SELF_PATH: &str = "/system/bin/test_rs_isa_grant";
const CLAIM_LABEL: &str = "isa-claim";

/// The i8042's data and command ports, keyboard and aux lines.
const I8042: &str = "0060,0064:1,12";
const DATA: u16 = 0x60;
const STATUS: u16 = 0x64;
/// The port just past the data port, which no row names.
const PAST: u16 = 0x61;

const OBF: u8 = 1 << 0;
const IBF: u8 = 1 << 1;

/// A controller answers a command in microseconds; this is the liveness
/// ceiling on one that does not, and it panics by name.
const CONTROLLER: Duration = Duration::from_secs(1);
/// The host types once it reads the ready line; a record that has not come in
/// this long is not coming.
const KEYSTROKE: Duration = Duration::from_secs(20);

/// Set 1's make and break codes for `a`: the controller translates the
/// keyboard's set 2.
const A_MAKE: u8 = 0x1E;
const A_BREAK: u8 = 0x9E;

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("driven") => driven(&syscap()),
        Some("grant") => grant(&syscap()),
        Some("device") => {
            device(claim(&syscap(), I8042).expect("isa device: the kernel gave the controller up at boot"))
        }
        Some("straddled") => device(claim_once_let_go(&syscap())),
        Some("released") => println!("isa released: the claim before this one is gone"),
        Some("unbound") => unbound(),
        Some("bound") => bound(),
        Some("wide") => wide(),
        Some("unclaimed") => unclaimed(),
        Some("moved") => moved(),
        other => panic!("isa_grant: unknown role {other:?}"),
    }
}

fn syscap() -> SysCap {
    Endowments::get()
        .take(SYSCAP_LABEL)
        .expect("the test estate is endowed a device-minting capability")
}

fn set(text: &str) -> IsaId {
    IsaId::parse(text).unwrap_or_else(|| panic!("{text:?} is no ISA set"))
}

fn claim(cap: &SysCap, text: &str) -> Result<Device, SyscallError> {
    cap.claim_isa(set(text))
}

fn inb(port: u16) -> u8 {
    let value: u8;
    // SAFETY: an `in` has no memory effect; a port this process holds no grant
    // for faults it, which is what the refusing roles exist to show.
    unsafe {
        core::arch::asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack));
    }
    value
}

fn inw(port: u16) -> u16 {
    let value: u16;
    // SAFETY: as `inb`; the access spans `port` and the port after it.
    unsafe {
        core::arch::asm!("in ax, dx", in("dx") port, out("ax") value, options(nomem, nostack));
    }
    value
}

fn outb(port: u16, value: u8) {
    // SAFETY: as `inb`; the ports written are the i8042's, granted to this process.
    unsafe {
        core::arch::asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack));
    }
}

fn driven(cap: &SysCap) {
    match claim(cap, I8042) {
        Err(SyscallError::PermissionDenied) => {}
        other => panic!(
            "isa: the kernel drives the i8042 and a claim on it answered {:?}",
            other.map(|_| ())
        ),
    }
    println!("isa: the claim on a controller the kernel drives was refused PermissionDenied");
}

/// Read the claim's description, which binds its ports to this process, and
/// check it names what was claimed.
fn bind(claim: &Device) {
    let mut words = [0u8; 16];
    let n = claim.read(&mut words).expect("isa: the claim's first read is its description");
    assert_eq!(n, words.len(), "isa: a description of {n} bytes");
    let wire = [
        u64::from_ne_bytes(words[..8].try_into().expect("eight bytes")),
        u64::from_ne_bytes(words[8..].try_into().expect("eight bytes")),
    ];
    assert_eq!(IsaId::from_wire(wire), Some(set(I8042)), "isa: the description names another set");
}

/// Spawn `role` holding `claim`, or nothing; answers its stdout and whether it
/// exited cleanly.
fn child(role: &str, claim: Option<Device>) -> (String, bool) {
    let mut command = Command::new(SELF_PATH);
    command.arg(role).stdout(Stdio::piped());
    if let Some(claim) = claim {
        command.endow(CLAIM_LABEL, claim.into_raw().0);
    }
    let out = command.output().expect("isa: spawn a child role");
    (String::from_utf8_lossy(&out.stdout).into_owned(), out.status.success())
}

/// A child that must have printed `marker` and then died at the access after it.
fn killed_after(role: &str, (said, clean): (String, bool), marker: &str) {
    assert!(said.contains(marker), "isa: {role} never reached its access: {said:?}");
    assert!(!clean && !said.contains("survived"), "isa: {role} survived its access: {said:?}");
    println!("isa: {role} was ended at {marker:?}");
}

fn grant(cap: &SysCap) {
    // Nothing but a whole row is a grant: a subset, a missing line and a
    // neighbouring port are each no function this machine can hand out.
    for part in ["0060:1", "0060,0064:1", "0061,0064:1,12", "0064:12"] {
        match claim(cap, part) {
            Err(SyscallError::NotFound) => {}
            other => panic!("isa: {part:?} answered {:?}, want NotFound", other.map(|_| ())),
        }
    }
    println!("isa: a subset, a missing line and a neighbouring port were each refused NotFound");

    let held = claim(cap, I8042).expect("isa: the i8042 row is free on a machine with none");
    match claim(cap, I8042) {
        Err(SyscallError::AlreadyExists) => {}
        other => panic!("isa: a second claim answered {:?}, want AlreadyExists", other.map(|_| ())),
    }
    killed_after("unbound", child("unbound", Some(held)), "unbound: in from 0x64");

    // The claim died with its unbound holder, so the row is free again.
    let held = claim(cap, I8042).expect("isa: the row came back from a holder that never bound it");
    killed_after("bound", child("bound", Some(held)), "bound: in from 0x61");
    let held = claim(cap, I8042).expect("isa: the row came back from the bound child");
    killed_after("wide", child("wide", Some(held)), "wide: in of two bytes from 0x60");
    killed_after("unclaimed", child("unclaimed", None), "unclaimed: in from 0x60");

    // The ports went back with the process that bound them.
    let held = claim(cap, I8042).expect("isa: the row came back from a holder that bound it");
    bind(&held);
    let status = inb(STATUS);
    println!("isa: bound here, status reads {status:#04x}");
    killed_after("moved", child("moved", Some(held)), "moved: in from 0x64");
    // The claim is gone with the child, and the ports are still this process's.
    match claim(cap, I8042) {
        Err(SyscallError::AlreadyExists) => {}
        other => panic!(
            "isa: a claim while this process holds the bound ports answered {:?}",
            other.map(|_| ())
        ),
    }
    let _ = inb(STATUS);
    println!("isa: a moved claim carries nothing, and the ports stay with the process that bound them");
    println!("===ISA_GRANT_OK===");
}

fn taken() -> Device {
    Endowments::get().take(CLAIM_LABEL).expect("isa: this role is endowed the claim")
}

fn unbound() {
    let _claim = taken();
    println!("unbound: in from 0x64");
    let _ = inb(STATUS);
    println!("unbound: survived");
}

fn bound() {
    let claim = taken();
    bind(&claim);
    // A machine with no i8042 floats its ports: all ones.
    println!("bound: 0x64 read {:#04x}", inb(STATUS));
    println!("bound: in from 0x61");
    let _ = inb(PAST);
    println!("bound: survived");
}

fn wide() {
    let claim = taken();
    bind(&claim);
    println!("wide: in of two bytes from 0x60");
    let _ = inw(DATA);
    println!("wide: survived");
}

fn unclaimed() {
    println!("unclaimed: in from 0x60");
    let _ = inb(DATA);
    println!("unclaimed: survived");
}

fn moved() {
    let claim = taken();
    let mut buf = [0u8; 16];
    match claim.read(&mut buf) {
        Err(SyscallError::PermissionDenied) => println!("moved: its read was refused"),
        other => panic!("moved: a claim bound to another process answered {other:?}"),
    }
    println!("moved: in from 0x64");
    let _ = inb(STATUS);
    println!("moved: survived");
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
    outb(STATUS, byte);
}

/// Drive the controller through `claim` until a keystroke arrives as a record
/// and a byte.
fn device(claim: Device) {
    bind(&claim);
    // Whatever the kernel left behind.
    for _ in 0..32 {
        if inb(STATUS) & OBF == 0 {
            break;
        }
        let _ = inb(DATA);
    }
    assert_eq!(inb(STATUS) & OBF, 0, "isa device: the output buffer never drained");
    // Read the configuration byte, then write it back with the keyboard's line
    // on and its clock running (i8042: 0x20 reads it, 0x60 writes it).
    command(0x20);
    wait_status("answered its configuration", |s| s & OBF != 0);
    let config = inb(DATA);
    command(0x60);
    wait_status("took the configuration", |s| s & IBF == 0);
    outb(DATA, (config | 0x01) & !0x10);
    command(0xAE);
    println!("isa device: config {config:#04x}, keyboard line on");
    println!("===ISA_DEVICE_READY===");

    let poller = Poller::new(1);
    poller.watch(&claim, READABLE, 0);
    let mut woke = false;
    poller.wait(1, KEYSTROKE.as_nanos() as u64, |_| woke = true);
    assert!(woke, "isa device: no record in {KEYSTROKE:?} of the keystroke");
    let mut record = [0u8; DeviceIrqRecord::SIZE];
    let n = syscall::read_nonblock(claim.as_handle(), &mut record)
        .expect("isa device: a readable claim reads a record");
    assert_eq!(n, DeviceIrqRecord::SIZE, "isa device: a record of {n} bytes");
    let count = u32::from_ne_bytes(record);
    assert!(count >= 1, "isa device: a record counting {count}");
    wait_status("had the byte behind its interrupt", |s| s & OBF != 0);
    let byte = inb(DATA);
    assert_eq!(byte, A_MAKE, "isa device: the interrupt carried {byte:#04x}, not `a`'s make");
    // The key's release too, so the output buffer is empty when this claim
    // goes and the host's next key would raise the line.
    wait_status("had the key's release", |s| s & OBF != 0);
    let released = inb(DATA);
    assert_eq!(released, A_BREAK, "isa device: the release carried {released:#04x}, not `a`'s break");
    println!("isa device: {count} interrupt(s), scancode {byte:#04x}");
    println!("===ISA_DEVICE_OK===");
}

/// The claim on a controller the kernel drives, asked for until the kernel lets
/// it go: at least one refusal first, or the kernel never drove it here.
fn claim_once_let_go(cap: &SysCap) -> Device {
    println!("===ISA_CLAIMING===");
    let by = Instant::now() + KEYSTROKE;
    let mut refused = 0u32;
    let claim = loop {
        match claim(cap, I8042) {
            Ok(claim) => break claim,
            Err(SyscallError::PermissionDenied) => refused += 1,
            Err(other) => panic!("isa straddled: a claim answered {other:?}"),
        }
        assert!(
            Instant::now() < by,
            "isa straddled: the kernel never let the controller go in {KEYSTROKE:?}"
        );
        std::thread::yield_now();
    };
    assert!(refused > 0, "isa straddled: the first claim was granted, so the kernel never drove the controller");
    println!("isa straddled: {refused} claim(s) refused before the kernel let the controller go");
    claim
}
