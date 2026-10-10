//! The xHCI controller the kernel was told to leave alone (`xhci-leave=`),
//! claimed by this job and driven by `/system/bin/usbd`, which it starts
//! itself so it can end it: this job holds the claim, the port usbd answers
//! `inspect` on, and usbd's process.
//!
//! - no argument: usbd started, asked what it named once it says it has
//!   settled, asked again across a span in which nothing may wake it, killed,
//!   and started again on a fresh claim, which must name the same devices.
//!   Every answer is printed for the harness to judge.
//! - `fault`: the claim's function aimed, with no usbd, at an address outside
//!   every grant: the kernel's `DMA FAULT` record is the harness's to read,
//!   and the claim refusing its interrupt read from then on is this job's.
//! - `refused`: the claim, on a kernel that drives the function, refused.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::os::toyos::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use toyos::endow::{Endowments, DEV_PREFIX, SERVE_PREFIX, SYSCAP_LABEL};
use toyos::ipc::Connection;
use toyos::poller::{Poller, READABLE};
use toyos::syscap::SysCap;
use toyos::volatile::Window;
use toyos::PciDev;
use toyos_abi::syscall::{DeviceRequest, PciId, SyscallError};
use toyos_inspect::{Value, MSG_INSPECT, MSG_SNAPSHOT, USB};

const USBD: &str = "/system/bin/usbd";

/// The controllers a boot of this job leaves to a claim: QEMU's `qemu-xhci`
/// on the guest profile, and the T14's Type-C controller. A machine has one
/// of them; the other is absent.
const SPARES: [PciId; 2] = [PciId { vendor: 0x1b36, device: 0x000d }, PciId { vendor: 0x8086, device: 0x9a13 }];

/// How long usbd has to say it has settled. Policy, and loose: enumeration
/// is a few control transfers per device, and the guest may be slow.
const SETTLE_BOUND: Duration = Duration::from_secs(30);

/// How long a killed usbd's claim may take to come back: the kernel publishes
/// a process's end before its deferred releases have run
/// (`issues/deferred-release-outlives-its-syscall.md`), the supervisor's
/// `CLAIM_RETURN` for the same reason. A liveness guard.
const CLAIM_RETURN: Duration = Duration::from_secs(2);

/// The span across which a parked usbd may wake for nothing but an event:
/// the absence is what is measured, so the span is the instrument.
const PARKED_SPAN: Duration = Duration::from_millis(500);

/// What the fault arm aims the command ring at: a gigabyte past its one
/// grant, which nothing in the claim's domain maps.
const PAST_THE_GRANT: u64 = 1 << 30;

fn main() {
    let cap: SysCap = Endowments::get().take(SYSCAP_LABEL).expect("test-runner endows a device-minting capability");
    match std::env::args().nth(1).as_deref() {
        None => drive(&cap),
        Some("fault") => fault(&cap),
        Some("refused") => refused(&cap),
        Some(other) => panic!("usbd_spare: no arm {other:?}"),
    }
}

/// The one spare this machine has, claimed, or every claim's refusal.
fn claim(cap: &SysCap) -> (PciId, Result<PciDev, SyscallError>) {
    let mut found = None;
    for id in SPARES {
        match cap.claim_pci::<PciDev>(id) {
            Err(SyscallError::NotFound) => {}
            answered => {
                assert!(found.is_none(), "usbd_spare: two spares on one machine");
                found = Some((id, answered));
            }
        }
    }
    found.expect("usbd_spare: neither spare controller is on this machine")
}

fn spelled(id: PciId) -> String {
    let mut buf = [0u8; DeviceRequest::MAX_NAME];
    DeviceRequest::Pci(id).write_name(&mut buf).to_string()
}

/// usbd, started on `dev` and serving its port to this job, and the lines it
/// says.
struct Usbd {
    child: Child,
    said: mpsc::Receiver<String>,
    conn: toyos::port::Connector,
}

impl Usbd {
    fn start(id: PciId, dev: PciDev) -> Self {
        let (acceptor, conn) = toyos::port::create().expect("usbd_spare: a port");
        let mut child = Command::new(USBD)
            .endow(&format!("{DEV_PREFIX}{}", spelled(id)), toyos::AsHandle::as_handle(&dev).0)
            .endow(&format!("{SERVE_PREFIX}{}", USB.port), acceptor.into_raw().0)
            .stdout(Stdio::piped())
            .spawn()
            .expect("usbd_spare: spawn usbd");
        // Moved: the spawn holds them now.
        std::mem::forget(dev);
        let out = BufReader::new(child.stdout.take().expect("usbd's piped stdout"));
        let (tx, said) = mpsc::channel();
        std::thread::spawn(move || {
            for line in out.lines() {
                let Ok(line) = line else { return };
                println!("usbd_spare: usbd said: {line}");
                if tx.send(line).is_err() {
                    return;
                }
            }
        });
        Self { child, said, conn }
    }

    /// Wait for usbd's line starting `prefix`, bounded.
    fn until(&self, prefix: &str) -> String {
        let deadline = Instant::now() + SETTLE_BOUND;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.said.recv_timeout(left) {
                Ok(line) if line.starts_with(prefix) => return line,
                Ok(_) => {}
                Err(why) => panic!("usbd_spare: usbd never said {prefix:?} within {SETTLE_BOUND:?}: {why:?}"),
            }
        }
    }

    /// One `inspect` answer, printed whole.
    fn ask(&self, when: &str) -> BTreeMap<String, Value> {
        let ns = toyos::namespace::build().add(USB.port, &self.conn).finish().expect("usbd_spare: a namespace");
        let conn: Connection = ns.open(USB.port).expect("usbd_spare: a connection to usbd");
        conn.signal(MSG_INSPECT).expect("usbd_spare: the request");
        let header = conn.recv_header().expect("usbd_spare: usbd's answer");
        assert_eq!(header.msg_type, MSG_SNAPSHOT, "usbd_spare: usbd answered something that is not a snapshot");
        let mut buf = vec![0u8; header.len() as usize];
        let n = conn.recv_bytes(&header, &mut buf).expect("usbd_spare: the snapshot's bytes");
        let snapshot = toyos_inspect::decode(&buf[..n], USB).expect("usbd_spare: a snapshot for `usb`");
        for (path, value) in &snapshot {
            println!("usbd_spare: {when}: {}", toyos_inspect::line(path, value));
        }
        snapshot
    }

    fn kill(mut self) {
        self.child.kill().expect("usbd_spare: kill usbd");
        let status = self.child.wait().expect("usbd_spare: wait for usbd");
        assert!(!status.success(), "usbd_spare: a killed usbd ended {status:?}");
    }
}

fn number(snapshot: &BTreeMap<String, Value>, path: &str) -> u64 {
    match snapshot.get(path) {
        Some(Value::U64(n)) => *n,
        other => panic!("usbd_spare: `usb.{path}` is {other:?}, not a count"),
    }
}

/// The devices an answer names, as `port id speed class` lines.
fn devices(snapshot: &BTreeMap<String, Value>) -> Vec<String> {
    let mut named = Vec::new();
    for (path, value) in snapshot {
        let Some(port) = path.strip_prefix("usb.device.").and_then(|p| p.strip_suffix(".id")) else { continue };
        let field = |f: &str| snapshot.get(&format!("usb.device.{port}.{f}")).map(|v| v.to_string()).unwrap_or_default();
        named.push(format!("port {port} {value} {} {}", field("speed"), field("class")));
    }
    named
}

fn drive(cap: &SysCap) {
    let (id, claimed) = claim(cap);
    let dev = claimed.unwrap_or_else(|why| panic!("usbd_spare: the claim on {} was refused: {why:?}", spelled(id)));
    let usbd = Usbd::start(id, dev);
    usbd.until("usbd: settled");
    let first = usbd.ask("settled");
    let named = devices(&first);
    assert_eq!(number(&first, "usb.devices") as usize, named.len());
    assert_eq!(number(&first, "usb.events.unannounced"), 0, "usbd_spare: an event no interrupt announced");
    assert_eq!(number(&first, "usb.operations.silent"), 0, "usbd_spare: an operation nothing answered");
    assert!(number(&first, "usb.interrupts.records") > 0, "usbd_spare: usbd read no interrupt record");
    assert_eq!(first.get("usb.noop").map(Value::to_string).as_deref(), Some("completed"));

    // Parked: across the span nothing wakes it that is not an event — no
    // timeout, and no claim read ready without a record behind it.
    let before = usbd.ask("parked, before");
    std::thread::sleep(PARKED_SPAN);
    let after = usbd.ask("parked, after");
    for path in ["usb.wakes.timed", "usb.wakes.empty"] {
        assert_eq!(number(&before, path), number(&after, path), "usbd_spare: {path} moved while usbd had nothing to do");
    }
    assert_eq!(after.get("usb.deadline_ns").map(Value::to_string).as_deref(), Some("none"));
    usbd.kill();
    println!("usbd_spare: usbd killed");

    let asked = Instant::now();
    let dev = loop {
        match cap.claim_pci::<PciDev>(id) {
            Err(SyscallError::AlreadyExists) if asked.elapsed() < CLAIM_RETURN => std::thread::sleep(Duration::from_millis(1)),
            answered => break answered.unwrap_or_else(|why| panic!("usbd_spare: the claim did not come back: {why:?}")),
        }
    };
    println!("usbd_spare: the claim came back after {} ms", asked.elapsed().as_millis());
    let again = Usbd::start(id, dev);
    again.until("usbd: settled");
    let second = again.ask("restarted");
    assert_eq!(devices(&second), named, "usbd_spare: a restarted usbd named other devices");
    again.kill();
    println!("usbd_spare: {} device(s) named, the same after a restart: {named:?}", named.len());
}

fn fault(cap: &SysCap) {
    let (id, claimed) = claim(cap);
    let dev = claimed.unwrap_or_else(|why| panic!("usbd_spare: the claim on {} was refused: {why:?}", spelled(id)));
    let info = dev.describe().expect("usbd_spare: the claim's description");
    let bar = dev.map_bar(0, info.bar_bytes[0]).expect("usbd_spare: BAR 0");
    // SAFETY: the mapping is the BAR's length and lives as long as `bar`.
    let regs = unsafe { Window::new(bar.as_ptr(), info.bar_bytes[0] as usize) };
    // One page granted, which is what starts the function mastering the bus.
    let grant = dev.dma_alloc(4096).expect("usbd_spare: a grant");
    let op = usize::from(regs.read::<u8>(0));
    let db = (regs.read::<u32>(0x14) & !3) as usize;
    let settle = |what: &str, done: &dyn Fn() -> bool| {
        let began = Instant::now();
        while !done() {
            assert!(began.elapsed() < Duration::from_secs(2), "usbd_spare: the controller {what}");
        }
    };
    regs.write::<u32>(op, 0);
    settle("never halted", &|| regs.read::<u32>(op + 4) & 1 != 0);
    regs.write::<u32>(op, 1 << 1);
    settle("held its reset", &|| regs.read::<u32>(op) & (1 << 1) == 0 && regs.read::<u32>(op + 4) & (1 << 11) == 0);
    let aimed = grant.device_addr + PAST_THE_GRANT;
    // The command ring there, the controller running, and its doorbell rung:
    // its first fetch is a read of an address no grant maps.
    regs.write::<u64>(op + 0x18, aimed | 1);
    regs.write::<u32>(op, 1);
    regs.write::<u32>(db, 0);
    println!("usbd_spare: the command ring aimed at {aimed:#x}, past the grant at {:#x}", grant.device_addr);

    let poller = Poller::new(1);
    poller.watch(&dev, READABLE, 0);
    poller.wait(1, Duration::from_secs(10).as_nanos() as u64, |_| {});
    match dev.irq() {
        Err(SyscallError::Io) => println!("usbd_spare: the claim refuses its interrupt read: Io"),
        Ok(record) => panic!("usbd_spare: after the aimed fetch the claim answered a record of {}, not the unit's refusal", record.count),
        Err(why) => panic!("usbd_spare: after the aimed fetch the claim answered {why:?}, not the unit's refusal"),
    }
}

fn refused(cap: &SysCap) {
    let (id, claimed) = claim(cap);
    match claimed {
        Err(SyscallError::PermissionDenied) => println!("usbd_spare: the claim on {} was refused: PermissionDenied", spelled(id)),
        Err(why) => panic!("usbd_spare: the claim was refused, by {why:?} and not because the kernel drives it"),
        Ok(_) => panic!("usbd_spare: the claim on {} was granted, on a kernel that drives it", spelled(id)),
    }
}
