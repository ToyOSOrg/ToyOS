//! A service a process adopted is its own: a helper it spawns is given a
//! duplicate of each service it was endowed and of none it adopted.
//!
//! The supervisor is the one process no parent endows a service. It adopts
//! a directory capability for every directory of the tree to read its own
//! files, and starts a storage row with no view; that row must hold no file
//! grant. The adopter here is that shape: given no service, it adopts this
//! test's `fs:/system`, reads its helper's image through it, and the helper
//! reports holding no service at all.
//!
//! A guest test because it runs the supervisor's path whole: std's adoption,
//! a spawn whose image is read through an adopted service, and the names the
//! helper reads back.

use std::os::toyos::process::CommandExt;
use std::process::Command;

use toyos::endow::Endowments;
use toyos::port::Connector;

const SELF: &str = "/system/bin/test_rs_adopted_services_stay";
/// The directory capability the adopter adopts, which its helper's image is read through.
const SYSTEM: &str = "fs:/system";
/// What the adopter is endowed that capability as: no service's name.
const GIVEN: &str = "files";
/// What a helper exits with once it has found itself holding no service.
const HELD_NONE: i32 = 43;

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("adopter") => adopter(),
        Some("helper") => helper(),
        Some(other) => panic!("unknown role {other:?}"),
        None => test(),
    }
}

fn test() {
    let system = toyos::endow::connector(SYSTEM)
        .expect("a test is endowed /system")
        .try_clone_to_owned()
        .expect("a copy of /system's capability");
    let status = Command::new(SELF)
        .arg("adopter")
        .services_only(&[])
        .endow(GIVEN, system.into())
        .status()
        .expect("spawn the adopter");
    assert!(status.success(), "the adopter failed: {status:?}");
    println!("adopted_services_stay: a helper of a process that adopted its services holds none of them");
}

fn adopter() {
    assert_eq!(toyos::endow::services().count(), 0, "the adopter was endowed a service");
    let system: Connector = Endowments::get().take(GIVEN).expect("the adopter is given /system");
    // SAFETY: a connector, as this process was endowed it.
    unsafe { std::os::toyos::fs::adopt_services(vec![(SYSTEM.to_string(), system.into())]) }.expect("the adopter adopts /system");
    // Non-vacuity: std reads the helper's image through `/system`, which it
    // holds only by adopting it.
    let status = Command::new(SELF).arg("helper").status().expect("spawn the helper through the adopted /system");
    assert_eq!(status.code(), Some(HELD_NONE), "the helper held a service: {status:?}");
}

fn helper() {
    let held: Vec<&str> = toyos::endow::services().collect();
    assert!(held.is_empty(), "a helper of an adopter holds {held:?}");
    std::process::exit(HELD_NONE);
}
