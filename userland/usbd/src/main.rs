//! usbd: one xHCI controller, driven from userland.
//!
//! **It holds a claim on one PCI function and nothing else of the machine.**
//! The claim arrives under the `dev:pci:` label its starter minted it under,
//! and the port it answers `inspect` on under `serve:usb`; the kernel keeps
//! config space, the interrupt vector and the function's address space, and
//! usbd never names an address its grant did not answer (`hc`).
//!
//! **One thread waits on one poller and nowhere else**: the claim's interrupt
//! records, the port's connections, and the next deadline the ports or the
//! outstanding operation hand back (`bus`). With none of those, it is parked
//! until one of the first two wakes it. Bring-up alone spins, on register bits
//! the controller raises no interrupt for, each bounded.
//!
//! **What it does with a controller** is the §4.22.1 handoff, a reset, its
//! rings in one grant, a No-Op that says the command ring and the interrupt
//! both work, and every port's device enumerated and named; no class is bound.
//!
//! **A server never blocks on a client.** Accept and the request are two
//! events, the request is a bare header buffered until whole, and the answer
//! is one `try_send` whose refusal drops the client by name.

use std::time::{Duration, Instant};

use toyos::endow::{self, Endowments};
use toyos::ipc::{Connection, FrameRx, RxStep};
use toyos::poller::{Poller, READABLE};
use toyos::AsHandle;
use toyos_abi::syscall::{SyscallError, DEV_PREFIX};
use toyos_inspect::{Snapshot, MSG_INSPECT, MSG_SNAPSHOT};

mod bus;
mod hc;

use bus::Bus;

/// Connections accepted and not yet asked, and how long one may stay so.
const MAX_PENDING: usize = 8;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);

const TOKEN_IRQ: u64 = 0;
const TOKEN_ACCEPT: u64 = 1;
const TOKEN_PENDING: u64 = 0x1_0000;

/// What woke the loop, which `inspect` reads: a loop with no deadline wakes
/// only for the claim or a client.
#[derive(Default)]
struct Wakes {
    /// The wait ended on its timeout.
    timed: u64,
    /// The claim read ready.
    device: u64,
    /// The claim read ready with no record behind it.
    empty: u64,
    /// A client connected or spoke.
    client: u64,
}

struct Pending {
    conn: Connection,
    rx: FrameRx<0>,
    since: Instant,
}

/// The one `dev:pci:` claim this process was started with.
fn claim() -> toyos::PciDev {
    let label = Endowments::get()
        .labels()
        .find(|l| l.strip_prefix(DEV_PREFIX).is_some_and(|name| name.starts_with("pci:")))
        .map(str::to_string)
        .unwrap_or_else(|| panic!("usbd: started with no `{DEV_PREFIX}pci:` claim"));
    Endowments::get().take(&label).expect("usbd: the claim its label names")
}

fn main() {
    let began = Instant::now();
    let now = || began.elapsed().as_nanos() as u64;
    let acceptor = endow::acceptor(toyos_inspect::USB.port)
        .unwrap_or_else(|| panic!("usbd: started serving no `{}` port", toyos_inspect::USB.port));
    let ctrl = hc::Controller::open(claim())
        .unwrap_or_else(|why| panic!("usbd: the controller this process was given cannot be driven — {why}"));
    let mut bus = Bus::new(ctrl, now());

    let poller = Poller::new(2 + MAX_PENDING as u32);
    let mut pending: Vec<Pending> = Vec::new();
    let mut wakes = Wakes::default();
    let mut ready: Vec<u64> = Vec::new();
    // Bring-up left the No-Op outstanding, so the first quiet is a change.
    let mut quiet = false;
    loop {
        let wake_at = bus.pass(now());
        if wake_at.is_none() && !quiet {
            println!("usbd: settled, {} device(s) named", bus.devices.len());
        }
        quiet = wake_at.is_none();
        poller.watch(&bus.ctrl.dev, READABLE, TOKEN_IRQ);
        poller.watch(&acceptor, READABLE, TOKEN_ACCEPT);
        for p in &pending {
            poller.watch(&p.conn, READABLE, TOKEN_PENDING + u64::from(p.conn.as_handle().0));
        }
        let mut timeout = wake_at.map_or(u64::MAX, |at| at.saturating_sub(now()));
        if let Some(first) = pending.iter().map(|p| p.since).min() {
            let left = HANDSHAKE_TIMEOUT.saturating_sub(first.elapsed());
            timeout = timeout.min(left.as_nanos() as u64);
        }
        ready.clear();
        poller.wait(1, timeout, |token| ready.push(token));
        if ready.is_empty() {
            wakes.timed += 1;
        }

        // **The record is taken, not merely noticed**: a claim reads ready
        // while it holds one, so one left would wake every wait after it.
        if ready.contains(&TOKEN_IRQ) {
            wakes.device += 1;
            match bus.ctrl.dev.irq() {
                Ok(record) => bus.interrupt(u64::from(record.count)),
                Err(SyscallError::WouldBlock) => wakes.empty += 1,
                // The kernel's word that the function is no longer this
                // process's: a fault at the unit is the one that happens.
                Err(why) => panic!("usbd: the claim refused its interrupt record ({why:?}); nothing drives it from here"),
            }
        }

        if ready.contains(&TOKEN_ACCEPT) {
            wakes.client += 1;
            match acceptor.accept() {
                Ok(conn) if pending.len() < MAX_PENDING => pending.push(Pending { conn, rx: FrameRx::new(), since: Instant::now() }),
                Ok(conn) => println!("usbd: refusing client {} — {MAX_PENDING} are already waiting", conn.as_handle().0),
                Err(why) => println!("usbd: the kernel refused a client's connection: {why:?}"),
            }
        }
        let mut i = 0;
        while i < pending.len() {
            let token = TOKEN_PENDING + u64::from(pending[i].conn.as_handle().0);
            if pending[i].since.elapsed() >= HANDSHAKE_TIMEOUT {
                println!("usbd: dropping client {} — it never asked", pending[i].conn.as_handle().0);
                pending.remove(i);
                continue;
            }
            if !ready.contains(&token) {
                i += 1;
                continue;
            }
            wakes.client += 1;
            let p = &mut pending[i];
            match p.rx.pump(&p.conn) {
                RxStep::Idle => i += 1,
                RxStep::Eof => {
                    pending.remove(i);
                }
                RxStep::Frame { msg_type: MSG_INSPECT, payload_len: 0 } => {
                    let p = pending.remove(i);
                    answer(&p.conn, &bus, &wakes, wake_at, now());
                }
                RxStep::Frame { .. } | RxStep::Malformed => {
                    println!("usbd: dropping client {} — it asked something that is not `inspect`", p.conn.as_handle().0);
                    pending.remove(i);
                }
            }
        }
    }
}

fn answer(conn: &Connection, bus: &Bus, wakes: &Wakes, wake_at: Option<u64>, now: u64) {
    let mut snap = Snapshot::new(toyos_inspect::USB);
    let (bus_no, dev, func) = bus.ctrl.at;
    snap.put("controller", format!("{bus_no:02x}:{dev:02x}.{func}"));
    snap.put("ports", u32::from(bus.ctrl.max_ports));
    snap.put("devices", bus.devices.len());
    for named in &bus.devices {
        let at = format!("device.{}", named.port + 1);
        let class = named.class();
        snap.put(&format!("{at}.id"), format!("{:04x}:{:04x}", named.device.vendor, named.device.product));
        snap.put(&format!("{at}.speed"), bus::speed_name(named.speed));
        snap.put(&format!("{at}.class"), format!("{:02x}:{:02x}:{:02x}", class.class, class.subclass, class.protocol));
        snap.put(&format!("{at}.class_name"), class.name().unwrap_or("unnamed"));
        snap.put(&format!("{at}.slot"), u32::from(named.slot));
    }
    let counts = &bus.counts;
    snap.put("noop", match counts.noop {
        None => "outstanding",
        Some(true) => "completed",
        Some(false) => "unanswered",
    });
    snap.put("interrupts.records", counts.records);
    snap.put("interrupts.counted", counts.interrupts);
    snap.put("events.taken", counts.events);
    snap.put("events.unannounced", counts.unannounced);
    snap.put("operations.silent", counts.silent);
    snap.put("wakes.timed", wakes.timed);
    snap.put("wakes.device", wakes.device);
    snap.put("wakes.empty", wakes.empty);
    snap.put("wakes.client", wakes.client);
    snap.put("deadline_ns", wake_at.map_or("none".to_string(), |at| at.saturating_sub(now).to_string()));
    let encoded = snap.encode().unwrap_or_else(|why| panic!("usbd: its snapshot: {why}"));
    if let Err(why) = conn.try_send_bytes(MSG_SNAPSHOT, &encoded) {
        println!("usbd: dropping client {} — its answer would not go: {why:?}", conn.as_handle().0);
    }
}
