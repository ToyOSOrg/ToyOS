//! A reader without an owner's connector cannot inspect that owner.
//!
//! Two arms, one binary, one boot and one running netd, and the only thing that
//! differs between them is whether the namespace handed to `/system/bin/inspect`
//! carries `netd`. The granted arm is what gives the denied one teeth: a reader
//! that could not reach netd for any other reason — netd down, the protocol
//! broken, the reader missing — fails the first arm instead of passing the
//! second.
//!
//! The denied child keeps every other owner's connector, so what it is refused
//! is exactly the one name it lacks and not a namespace that reaches nothing.
//!
//! The kernel's inventory is the same pair on a capability: `dev.*` read with
//! a duplicate carrying `Rights::INVENTORY` and with one narrowed to lack it.

use std::os::toyos::process::CommandExt;
use std::process::{Command, Output, Stdio};

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::syscap::SysCap;
use toyos::{endow, namespace};
use toyos_abi::handle::Rights;
use toyos_abi::syscall::SVC_LABEL;

const READER: &str = "/system/bin/inspect";

/// `inspect net.*`, holding the named connectors out of this process's own and
/// nothing else.
fn inspect_holding(names: &[&str]) -> Output {
    let base = endow::namespace().expect("test-runner hands its namespace down");
    let narrowed = namespace::build().keep(base, names).finish().expect("a narrower namespace");
    Command::new(READER)
        .arg("net.*")
        .endow(SVC_LABEL, narrowed.into_raw().0)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn /system/bin/inspect")
        .wait_with_output()
        .expect("wait for /system/bin/inspect")
}

fn main() {
    let granted = inspect_holding(&["netd"]);
    let out = String::from_utf8_lossy(&granted.stdout);
    let err = String::from_utf8_lossy(&granted.stderr);
    assert_eq!(granted.status.code(), Some(0), "granted: stdout {out:?} stderr {err:?}");
    assert!(out.lines().any(|l| l.starts_with("net.mac = ")), "granted: {out:?}");
    assert!(out.lines().all(|l| l.starts_with("net.")), "granted answered past net.*: {out:?}");

    let denied = inspect_holding(&["soundd", "log", "compositor"]);
    let out = String::from_utf8_lossy(&denied.stdout);
    let err = String::from_utf8_lossy(&denied.stderr);
    assert_eq!(denied.status.code(), Some(2), "denied: stdout {out:?} stderr {err:?}");
    assert!(out.is_empty(), "denied read netd anyway: {out:?}");
    assert!(
        err.contains("this program holds no `netd` connector"),
        "denied was refused for another reason: {err:?}"
    );
    println!("inspect denied: granted read netd, denied was refused by name");

    the_inventory_is_a_right();
}

/// `inspect dev.*` with a capability carrying `INVENTORY` and with one that
/// does not: the kernel's own refusal is the only difference, and the reader
/// has to name it.
fn the_inventory_is_a_right() {
    let cap: SysCap = Endowments::get()
        .take(SYSCAP_LABEL)
        .expect("test-runner endows every binary it spawns a system capability");
    let dev = |rights: Rights| -> Output {
        let narrowed = cap.narrowed(rights).expect("a narrower capability");
        Command::new(READER)
            .arg("dev.*")
            .endow(SYSCAP_LABEL, narrowed.into_raw().0)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn /system/bin/inspect")
            .wait_with_output()
            .expect("wait for /system/bin/inspect")
    };

    let granted = dev(Rights::TRANSFER.union(Rights::INVENTORY));
    let out = String::from_utf8_lossy(&granted.stdout);
    let err = String::from_utf8_lossy(&granted.stderr);
    assert_eq!(granted.status.code(), Some(0), "granted: stdout {out:?} stderr {err:?}");
    assert!(out.lines().any(|l| l.starts_with("dev.cpus = ")), "granted: {out:?}");
    assert!(out.lines().all(|l| l.starts_with("dev.")), "granted answered past dev.*: {out:?}");

    let denied = dev(Rights::TRANSFER);
    let out = String::from_utf8_lossy(&denied.stdout);
    let err = String::from_utf8_lossy(&denied.stderr);
    assert_eq!(denied.status.code(), Some(2), "denied: stdout {out:?} stderr {err:?}");
    assert!(out.is_empty(), "denied read the inventory anyway: {out:?}");
    assert!(
        err.contains("does not carry `inventory`"),
        "denied was refused for another reason: {err:?}"
    );
    println!("inventory denied: granted read dev.*, denied was refused by the kernel");
}
