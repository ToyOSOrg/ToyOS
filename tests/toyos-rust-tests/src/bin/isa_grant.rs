//! The `isa` claim's ports: the i8042's two, opened in the I/O permission
//! bitmap for the one process that bound them and for no other.
//!
//! Run where the kernel drives no i8042 (`i8042-withheld`), so the claim is
//! granted. Every port access that must be refused is made in a child of its
//! own, which prints a marker, makes the one access and must never print again;
//! the kernel's record of each kill is the harness's to read.
//!
//! The children: `unbound` holds the claim and never read it, `bound` read it
//! and steps one port past what it was granted, `wide` read it and makes a
//! two-byte access at its data port, `out` read it and writes one port past
//! the grant, `unclaimed` holds nothing, and `moved` was handed a claim its
//! parent had already bound.

use std::os::toyos::process::CommandExt;
use std::process::{Command, Stdio};

use toyos::endow::Endowments;
use toyos::syscap::SysCap;
use toyos::Device;
use toyos_abi::syscall::{IsaId, SyscallError, SYSCAP_LABEL};

#[path = "../arch/port.rs"]
mod port;

use port::{port_in, port_out};

const SELF_PATH: &str = "/system/bin/test_rs_isa_grant";
const CLAIM_LABEL: &str = "isa-claim";

/// The i8042's data and command ports, keyboard and aux lines.
const I8042: &str = "0060,0064:1,12";
const DATA: u16 = 0x60;
const STATUS: u16 = 0x64;
/// The port just past the data port, which no row names.
const PAST: u16 = 0x61;

fn main() {
    match std::env::args().nth(1).as_deref() {
        None => grant(&syscap()),
        Some("unbound") => unbound(),
        Some("bound") => bound(),
        Some("wide") => wide(),
        Some("out") => out(),
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

    let held = claim(cap, I8042).expect("isa: the i8042 row is free where the kernel drives none");
    match claim(cap, I8042) {
        Err(SyscallError::AlreadyExists) => {}
        other => panic!("isa: a second claim answered {:?}, want AlreadyExists", other.map(|_| ())),
    }
    killed_after("unbound", child("unbound", Some(held)), "unbound: in from 0x64");

    // The claim died with its unbound holder, so the row is free again; each
    // bound child's ports go back with it.
    for (role, marker) in [
        ("bound", "bound: in from 0x61"),
        ("wide", "wide: in of two bytes from 0x60"),
        ("out", "out: out to 0x61"),
    ] {
        let held = claim(cap, I8042).expect("isa: the row came back from the holder before");
        killed_after(role, child(role, Some(held)), marker);
    }
    killed_after("unclaimed", child("unclaimed", None), "unclaimed: in from 0x60");

    let held = claim(cap, I8042).expect("isa: the row came back from a holder that bound it");
    bind(&held);
    let status = port_in(STATUS, false);
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
    let _ = port_in(STATUS, false);
    println!("isa: a moved claim carries nothing, and the ports stay with the process that bound them");
}

fn taken() -> Device {
    Endowments::get().take(CLAIM_LABEL).expect("isa: this role is endowed the claim")
}

fn unbound() {
    let _claim = taken();
    println!("unbound: in from 0x64");
    let _ = port_in(STATUS, false);
    println!("unbound: survived");
}

fn bound() {
    let claim = taken();
    bind(&claim);
    println!("bound: 0x64 read {:#04x}", port_in(STATUS, false));
    println!("bound: in from 0x61");
    let _ = port_in(PAST, false);
    println!("bound: survived");
}

fn wide() {
    let claim = taken();
    bind(&claim);
    println!("wide: in of two bytes from 0x60");
    let _ = port_in(DATA, true);
    println!("wide: survived");
}

fn out() {
    let claim = taken();
    bind(&claim);
    println!("out: out to 0x61");
    port_out(PAST, 0);
    println!("out: survived");
}

fn unclaimed() {
    println!("unclaimed: in from 0x60");
    let _ = port_in(DATA, false);
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
    let _ = port_in(STATUS, false);
    println!("moved: survived");
}
