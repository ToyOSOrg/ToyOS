//! `/system/bin/virtio-console` — the driver of a virtio console device
//! (virtio 1.2 §5.3), which QEMU calls virtio-serial: one process per
//! function, holding its claim, and handing the function's one named port to
//! the service the name is for.
//!
//! **The port's name says whose it is**, and the row's `receives` are the
//! endpoints it may hand one to:
//! - [`CONSOLE_PORT`], the VM's console, carries `logkeeper`'s log as the
//!   console's carrier ([`toyos_logstream::CARRIER`]): [`console`].
//! - [`PORT_NAME`], QEMU's clipboard channel, goes to the clipboard's agent
//!   ([`toyos_vdagent::endpoint::SERVICE`]): [`agent`].
//!
//! A port whose service this image does not run ends the program with exit
//! 0: nothing here is to be served. So the driver holds the device and the
//! endpoints, and never the clipboard: each service keeps its own authority.

mod agent;
mod channel;
mod console;

use std::time::Instant;

use toyos::endow;
use toyos::poller::{Poller, READABLE};
use toyos::say;
use toyos::PciDev;
use toyos_abi::syscall::PciId;
use toyos_manifest::device::PciMatch;
use toyos_vdagent::PORT_NAME;
use toyos_virtio_serial::control::Control;
use toyos_virtio_serial::probe::{Probe, Step};
use toyos_virtio_serial::{CONSOLE_PORT, PORT_ID};

use channel::{Channel, PROBE_BOUND};

/// A communication controller of class `07:80:00`, which virtio 1.2 §5.3.1
/// numbers `0x1040 + 3`: the matcher its row drives, and the label its claim
/// is endowed under.
const VIRTIO_SERIAL: PciMatch = PciMatch::new([0x07, 0x80, 0x00], Some(PciId { vendor: 0x1af4, device: 0x1043 }));

fn main() {
    let claim: PciDev = endow::claim(&VIRTIO_SERIAL.to_string())
        .expect("virtio-console: its row drives a function, and it is started holding the function's claim");
    let mut channel =
        Channel::open(claim).unwrap_or_else(|why| panic!("virtio-console: the function is not driven: {why}"));
    let poller = Poller::new(3);
    let mut probe = Probe::default();
    let held = identify(&mut channel, &mut probe, &poller);
    let name = probe.name().expect("identified").to_string();
    match name.as_str() {
        CONSOLE_PORT => console::run(channel, probe, held, poller),
        PORT_NAME => agent::run(channel, probe, held, poller),
        other => {
            say!("virtio-console: port {PORT_ID} is {other:?}, which no service here is for");
            std::process::exit(0);
        }
    }
}

/// Read the device's control messages, answering each `DEVICE_ADD`, until it
/// has named [`PORT_ID`]: answers the messages about it that came with the
/// name, the port's driver's to answer. A device that misnames its ports, or
/// names none within [`PROBE_BOUND`], ends the program by name.
fn identify(channel: &mut Channel, probe: &mut Probe, poller: &Poller) -> Vec<Control> {
    let deadline = Instant::now() + PROBE_BOUND;
    let mut held = Vec::new();
    loop {
        while let Some(received) = channel.control() {
            match probe.take(received.bytes()) {
                Ok(Step::Reply(reply)) => channel.send_control(reply),
                Ok(Step::Driver(control)) => held.push(control),
                Ok(Step::Nothing) => {}
                Err(why) => panic!("virtio-console: {why}"),
            }
        }
        if probe.name().is_some() {
            return held;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            panic!("virtio-console: the device named no port {PORT_ID} within {PROBE_BOUND:?}");
        }
        poller.watch(channel.claim(), READABLE, 0);
        poller.wait(1, u64::try_from(left.as_nanos()).unwrap_or(u64::MAX), |_| {});
        channel.take_interrupt();
    }
}

/// Nanoseconds to wait until `at` plus `bound`, for a poller's timeout.
fn until(at: Instant, bound: std::time::Duration) -> u64 {
    let left = (at + bound).saturating_duration_since(Instant::now());
    u64::try_from(left.as_nanos()).unwrap_or(u64::MAX)
}
