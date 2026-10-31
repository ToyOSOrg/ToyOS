//! `usbd_spare`'s arms, one binary each for a metal job and all of them
//! behind one for QEMU. The xHCI controller the image's `drives` rows leave,
//! claimed by this job and driven by `/system/bin/usbd`, which it
//! starts itself so it can end it: this job holds the claim, the port usbd
//! answers `inspect` on, and usbd's process.
//!
//! - no argument: usbd started, asked once it says it has settled whether its
//!   No-Op was answered by an interrupt through the claim, asked again across
//!   a span in which nothing may wake it, and killed. Every answer is printed
//!   for the harness to judge.
//! - `fault`: the claim's function aimed, with no usbd, at an address outside
//!   every grant: the kernel's `DMA FAULT` record is the harness's to read,
//!   and the claim refusing its interrupt read from then on is this job's.
//! - `refused`: a claim on the boot controller, which the image's usbd
//!   holds, refused.
//! - `unhanded`: the claim, on a machine that will not hand it over,
//!   refused; the kernel's line says why.
//! - `vector`: the claim's MSI-X table entry rewritten, by this job through
//!   the BAR that holds it, to a vector that is not the claim's, and usbd
//!   started: its No-Op's message is the unit's to refuse and record, which
//!   puts the claim in refusal, so usbd ends on it never told of its No-Op.

#[path = "spare.rs"]
mod spare;

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
use toyos_abi::syscall::{PciId, SyscallError};
use toyos_inspect::{Value, MSG_INSPECT, MSG_SNAPSHOT, USB};

const USBD: &str = "/system/bin/usbd";

/// The device-minting capability the test runner endows every job with.
pub fn cap() -> SysCap {
    Endowments::get().take(SYSCAP_LABEL).expect("test-runner endows a device-minting capability")
}

/// The controllers a boot of this job can leave to a claim: QEMU's
/// `qemu-xhci` on the guest profiles, either of the T14's two and the AMD
/// bench's, whose two controllers share an id. `claim` takes the first of them
/// nobody holds.
const SPARES: [PciId; 4] = [
    PciId { vendor: 0x8086, device: 0xa0ed },
    PciId { vendor: 0x1b36, device: 0x000d },
    PciId { vendor: 0x8086, device: 0x9a13 },
    PciId { vendor: 0x1022, device: 0x1639 },
];

/// How long usbd has to say it has settled. Policy, and loose: enumeration
/// is a few control transfers per device, and the guest may be slow.
const SETTLE_BOUND: Duration = Duration::from_secs(30);

/// The span across which a parked usbd may wake for nothing but an event:
/// the absence is what is measured, so the span is the instrument.
const PARKED_SPAN: Duration = Duration::from_millis(500);

/// What the fault arm aims the command ring at: a gigabyte past its one
/// grant, which nothing in the claim's domain maps.
const PAST_THE_GRANT: u64 = 1 << 30;

/// The claim on the spare this machine has: the one function of a spare's
/// card the kernel does not drive, or, on a kernel that drives every one,
/// the only one there is.
fn claim(cap: &SysCap) -> (PciId, Result<PciDev, SyscallError>) {
    let pci = spare::of(cap, &SPARES).expect("usbd_spare: no spare controller on this machine");
    (PciId { vendor: pci.vendor, device: pci.device }, cap.claim_function(pci.arrival))
}

fn spelled(id: PciId) -> String {
    format!("pci:{:04x}:{:04x}", id.vendor, id.device)
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
            .endow(&format!("{DEV_PREFIX}{}", spelled(id)), dev.into())
            .endow(&format!("{SERVE_PREFIX}{}", USB.port), acceptor.into())
            .stdout(Stdio::piped())
            .spawn()
            .expect("usbd_spare: spawn usbd");
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

    /// Wait for usbd's line saying `what`, bounded.
    fn until(&self, what: &str) -> String {
        let deadline = Instant::now() + SETTLE_BOUND;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.said.recv_timeout(left) {
                Ok(line) if line.starts_with("usbd ") && line.contains(what) => return line,
                Ok(_) => {}
                Err(why) => panic!("usbd_spare: usbd never said {what:?} within {SETTLE_BOUND:?}: {why:?}"),
            }
        }
    }

    /// Every line usbd says until it ends, bounded.
    fn until_ended(&self) -> Vec<String> {
        let deadline = Instant::now() + SETTLE_BOUND;
        let mut said = Vec::new();
        loop {
            match self.said.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(line) => said.push(line),
                Err(mpsc::RecvTimeoutError::Disconnected) => return said,
                Err(why) => panic!("usbd_spare: usbd did not end within {SETTLE_BOUND:?}: {why:?}"),
            }
        }
    }

    /// One `inspect` answer, printed whole.
    fn ask(&self, when: &str) -> BTreeMap<String, Value> {
        let ns = toyos::namespace::build().add(USB.port, &self.conn).finish().expect("usbd_spare: a namespace");
        let conn: Connection = ns.open(USB.port).expect("usbd_spare: a connection to usbd");
        conn.signal(MSG_INSPECT).expect("usbd_spare: the request");
        let mut rx = Box::new(toyos::ipc::FrameRx::<{ toyos::ipc::MAX_FRAME_LEN as usize }>::new());
        let frame = rx.recv(&conn).expect("usbd_spare: usbd's answer");
        assert_eq!(frame.msg_type, MSG_SNAPSHOT, "usbd_spare: usbd answered something that is not a snapshot");
        let snapshot = toyos_inspect::decode(rx.payload(frame.payload_len), USB).expect("usbd_spare: a snapshot for `usb`");
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

pub fn drive(cap: &SysCap) {
    let (id, claimed) = claim(cap);
    let dev = claimed.unwrap_or_else(|why| panic!("usbd_spare: the claim on {} was refused: {why:?}", spelled(id)));
    let usbd = Usbd::start(id, dev);
    usbd.until(": settled");
    let first = usbd.ask("settled");
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
}

pub fn fault(cap: &SysCap) {
    let (id, claimed) = claim(cap);
    let dev = claimed.unwrap_or_else(|why| panic!("usbd_spare: the claim on {} was refused: {why:?}", spelled(id)));
    let info = dev.describe().expect("usbd_spare: the claim's description");
    let bar = dev.map_bar(0, info.bar_bytes[0]).expect("usbd_spare: BAR 0");
    // SAFETY: the mapping is the BAR's length and lives as long as `bar`.
    let regs = unsafe { Window::new(bar.as_ptr(), info.bar_bytes[0] as usize) };
    // One page granted, and the function started: mastering is what its
    // aimed fetch needs.
    dev.dma_limit(64).expect("usbd_spare: the address limit");
    let grant = dev.dma_alloc(4096).expect("usbd_spare: a grant");
    dev.dma_start().expect("usbd_spare: the start");
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

/// QEMU's boot controller, which the image's usbd row drives.
const BOOT: [PciId; 1] = [PciId { vendor: 0x1033, device: 0x0194 }];

pub fn refused(cap: &SysCap) {
    let held: Vec<_> = spare::all(cap, &BOOT).into_iter().filter(|pci| pci.driven == toyos_abi::inventory::Driven::Claimed).collect();
    let [boot] = held[..] else { panic!("usbd_spare: not one boot controller a process holds: {} are", held.len()) };
    let id = PciId { vendor: boot.vendor, device: boot.device };
    match cap.claim_function::<PciDev>(boot.arrival) {
        Err(SyscallError::AlreadyExists) => println!("usbd_spare: the claim on {} was refused: AlreadyExists", spelled(id)),
        Err(why) => panic!("usbd_spare: the claim was refused, by {why:?} and not because usbd holds it"),
        Ok(_) => panic!("usbd_spare: the claim on {} was granted, while usbd holds it", spelled(id)),
    }
}

/// The claim's MSI-X table entry 0, mapped through the BAR that holds it, and
/// its offset there, off the function's own capability chain (PCI 3.0
/// §6.8.2: id 0x11, the table's BIR and offset in the dword at +4). Mapped
/// as far as that entry and no further, without the claim's description,
/// which is read once and is usbd's.
fn msix_table(dev: &PciDev) -> (toyos::shm::SharedMemory, usize) {
    use toyos_abi::syscall::RegWidth;
    let mut at = dev.config_read(0x34, RegWidth::U8).expect("usbd_spare: the capability pointer") & 0xFC;
    for _ in 0..48 {
        assert!(at != 0, "usbd_spare: the function has no MSI-X capability");
        let header = dev.config_read(at, RegWidth::U32).expect("usbd_spare: a capability header");
        if header & 0xFF == 0x11 {
            let table = dev.config_read(at + 4, RegWidth::U32).expect("usbd_spare: the MSI-X table word");
            let offset = (table & !7) as usize;
            let bar = dev
                .map_bar(table & 7, offset as u64 + 16)
                .unwrap_or_else(|why| panic!("usbd_spare: BAR {}, which holds the MSI-X table, was not handed over: {why:?}", table & 7));
            return (bar, offset);
        }
        at = (header >> 8) & 0xFC;
    }
    panic!("usbd_spare: the capability chain does not end");
}

pub fn vector(cap: &SysCap) {
    let (id, claimed) = claim(cap);
    let dev = claimed.unwrap_or_else(|why| panic!("usbd_spare: the claim on {} was refused: {why:?}", spelled(id)));
    let (bar, table) = msix_table(&dev);
    // SAFETY: the mapping is the BAR's length and lives as long as `bar`.
    let regs = unsafe { Window::new(bar.as_ptr(), bar.len()) };
    // Entry 0's data word, at +8 of the entry (§6.8.2.6).
    let data = regs.read::<u32>(table + 8);
    let foreign = data ^ 1;
    regs.write::<u32>(table + 8, foreign);
    println!("usbd_spare: MSI-X entry 0's data {data:#x} rewritten to {foreign:#x}, a vector not the claim's");
    let mut usbd = Usbd::start(id, dev);
    let said = usbd.until_ended();
    assert!(
        !said.iter().any(|line| line.starts_with("usbd ") && line.contains(": a No-Op command completed")),
        "usbd_spare: usbd was told of its No-Op through a vector not its own"
    );
    let status = usbd.child.wait().expect("usbd_spare: wait for usbd");
    assert!(!status.success(), "usbd_spare: usbd ended {status:?} on a claim the unit refused");
    println!("usbd_spare: usbd ended on the claim's refusal: it exited {status:?} and never said its No-Op completed");
}

pub fn unhanded(cap: &SysCap) {
    let (id, claimed) = claim(cap);
    match claimed {
        Err(why) => println!("usbd_spare: the claim on {} was refused: {why:?}", spelled(id)),
        Ok(_) => panic!("usbd_spare: the claim on {} was granted, on a machine that would not hand it over", spelled(id)),
    }
}
