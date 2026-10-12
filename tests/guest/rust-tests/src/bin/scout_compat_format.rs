//! Measurement scout, never landed: the job of the Wi-Fi design's S1 A/B.
//!
//! It claims the controller `xhci-leave=` leaves alone and starts
//! `/system/bin/usbd` on the claim, whose bring-up rings a No-Op the claim's
//! message announces. The boots differ in that message's format alone
//! (`claim-compat-format`); what the unit did with it is the kernel's records,
//! which the harness reads. This job says what usbd said and how it ended,
//! ends it if it is still running, and exits 0 either way: it is the stimulus,
//! not the verdict.

use std::io::{BufRead, BufReader};
use std::os::toyos::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use toyos::endow::{Endowments, DEV_PREFIX, SERVE_PREFIX, SYSCAP_LABEL};
use toyos::syscap::SysCap;
use toyos::PciDev;
use toyos_abi::syscall::{DeviceRequest, PciId, SyscallError};
use toyos_inspect::USB;

const USBD: &str = "/system/bin/usbd";

/// QEMU's `qemu-xhci` on the guest profile, and the T14's Type-C controller.
const SPARES: [PciId; 2] = [PciId { vendor: 0x1b36, device: 0x000d }, PciId { vendor: 0x8086, device: 0x9a13 }];

/// How long usbd has to settle or end: `usbd_spare`'s bound, a liveness guard.
const BOUND: Duration = Duration::from_secs(30);

fn main() {
    let cap: SysCap = Endowments::get().take(SYSCAP_LABEL).expect("test-runner endows a device-minting capability");
    let mut found = None;
    for id in SPARES {
        match cap.claim_pci::<PciDev>(id) {
            Err(SyscallError::NotFound) => {}
            answered => {
                assert!(found.is_none(), "scout_compat_format: two spares on one machine");
                found = Some((id, answered));
            }
        }
    }
    let (id, claimed) = found.expect("scout_compat_format: neither spare controller is on this machine");
    let mut buf = [0u8; DeviceRequest::MAX_NAME];
    let spelled = DeviceRequest::Pci(id).write_name(&mut buf).to_string();
    let dev = claimed.unwrap_or_else(|why| panic!("scout_compat_format: the claim on {spelled} was refused: {why:?}"));

    let (acceptor, _conn) = toyos::port::create().expect("scout_compat_format: a port");
    let mut child = Command::new(USBD)
        .endow(&format!("{DEV_PREFIX}{spelled}"), toyos::AsHandle::as_handle(&dev).0)
        .endow(&format!("{SERVE_PREFIX}{}", USB.port), acceptor.into_raw().0)
        .stdout(Stdio::piped())
        .spawn()
        .expect("scout_compat_format: spawn usbd");
    // Moved: the spawn holds it now.
    std::mem::forget(dev);
    let out = BufReader::new(child.stdout.take().expect("usbd's piped stdout"));
    let (tx, said) = mpsc::channel();
    std::thread::spawn(move || {
        for line in out.lines() {
            let Ok(line) = line else { return };
            println!("scout_compat_format: usbd said: {line}");
            if tx.send(line).is_err() {
                return;
            }
        }
    });

    let deadline = Instant::now() + BOUND;
    let settled = loop {
        match said.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(line) if line.starts_with("usbd: settled") => break true,
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break false,
            Err(mpsc::RecvTimeoutError::Timeout) => panic!("scout_compat_format: usbd neither settled nor ended within {BOUND:?}"),
        }
    };
    if settled {
        println!("scout_compat_format: usbd settled");
        child.kill().expect("scout_compat_format: kill usbd");
    } else {
        println!("scout_compat_format: usbd ended before it settled");
    }
    let status = child.wait().expect("scout_compat_format: wait for usbd");
    println!("scout_compat_format: usbd ended {status:?}");
}
