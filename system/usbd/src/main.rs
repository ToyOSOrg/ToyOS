//! usbd: one xHCI controller and every device on it, driven from userland.
//!
//! **It holds a claim on one PCI function** (`dev:pci:`), the acceptor it
//! answers `inspect` on, and what its row receives: the block service's
//! `disk` port, where its sticks are offered and their sessions handed over,
//! and the owner of input's `input` port, where its keyboards and pointers
//! report. The kernel keeps config space, the interrupt vector and the
//! function's address space; every decision is `toyos-xhci`'s, and this
//! binary is its boundary one instruction deep (`toyos-pci-claim`).
//!
//! **One thread waits on one poller and nowhere else**: the claim's
//! interrupt records, the block service's link and each session's doorbell,
//! the owner of input's connection while it is full, `inspect`'s clients, and
//! the next deadline the driver's tasks hand back.
//!
//! **The event ring is read only when an interrupt record says so**, so an
//! answer the controller wrote and no interrupt announced is found only by
//! its operation's deadline, and counted (`events.unannounced`).
//!
//! **A server never blocks on a client**: every answer is one `try_send`,
//! and the owner of input is never waited on (`toyos_xhci::input`).

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use toyos::endow::{self, EndowError, Endowments};
use toyos::ipc::{Connection, FrameRx, RxStep, TrySendError};
use toyos::poller::{Poller, READABLE, WRITABLE};
use toyos::{AsHandle, PciDev};
use toyos_abi::syscall::{SyscallError, DEV_PREFIX};
use toyos_blockring::disk::Settled;
use toyos_blockserve::link::{Handed, Link};
use toyos_blockserve::sessions::{Owner as Request, MAX_SESSIONS};
use toyos_blockserve::{table, Device, Sessions};
use toyos_input::wire::{self, Record, MAX_PAYLOAD};
use toyos_inspect::{Snapshot, MSG_INSPECT, MSG_SNAPSHOT};
use toyos_pci_claim::{Bar, Grant};
use toyos_xhci::exec::{Dead, Host, Nanos, Tasks};
use toyos_xhci::hc::Reset;
use toyos_xhci::input::{Opened, Owner, Sent};
use toyos_xhci::msc::{Event, Work};
use toyos_xhci::usb::{self, Config, Usb};

/// Connections accepted and not yet asked, and how long one may stay so.
const MAX_PENDING: usize = 8;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);

const TOKEN_IRQ: u64 = 0;
const TOKEN_ACCEPT: u64 = 1;
const TOKEN_LINK: u64 = 2;
const TOKEN_INPUT: u64 = 3;
const TOKEN_STOP: u64 = 4;
const TOKEN_PENDING: u64 = 0x1_0000;
/// A session's doorbell: its disk in bits 16 up, its id below.
const TOKEN_SESSION: u64 = 0x1_0000_0000;

/// The process the driver runs in: its clock, and its lines.
#[derive(Clone)]
struct Process {
    began: Instant,
    name: String,
}

impl Host for Process {
    fn now(&self) -> Nanos {
        self.began.elapsed().as_nanos() as Nanos
    }
    fn say(&self, line: core::fmt::Arguments<'_>) {
        println!("{}: {line}", self.name);
    }
}

type Driver = Usb<Bar, Grant, Process, table::Read>;

/// The owner of input, over the `input` port.
#[derive(Default)]
struct InputPort {
    conn: Option<Connection>,
}

impl Owner for InputPort {
    fn open(&mut self) -> Opened {
        match endow::service(wire::PORT) {
            Ok(conn) => {
                self.conn = Some(conn);
                Opened::Opened
            }
            Err(EndowError::NotEndowed) => Opened::NotGiven,
            Err(_) => Opened::Refused,
        }
    }
    fn send(&mut self, record: &Record) -> Sent {
        let Some(conn) = &self.conn else { return Sent::Ended };
        let mut payload = [0u8; MAX_PAYLOAD];
        let (msg_type, len) = record.frame(&mut payload);
        match conn.try_send_bytes(msg_type, &payload[..len]) {
            Ok(()) => Sent::Taken,
            // Nothing went: a frame is sent whole or not at all.
            Err(TrySendError::Full) => Sent::Full,
            Err(_) => Sent::Ended,
        }
    }
    fn close(&mut self) {
        self.conn = None;
    }
}

/// One disk, as its sessions reach the driver.
struct Disk<'a> {
    usb: &'a Driver,
    disk: u32,
    claim: &'a PciDev,
}

impl Device for Disk<'_> {
    fn has_room(&self) -> bool {
        self.usb.disks.has_room(self.disk)
    }
    fn submit_io(&mut self, write: bool, block: u64, blocks: u32, at: u64, owner: Request) {
        self.usb.disks.submit(self.disk, Work::Io { write, block, blocks, at, token: (owner.session, owner.tag) });
    }
    fn submit_flush(&mut self, owner: Request) -> bool {
        let cached = self.usb.disks.cached(self.disk);
        if cached {
            self.usb.disks.submit(self.disk, Work::Flush { token: (owner.session, owner.tag) });
        }
        cached
    }
    fn dma_map(&mut self, region: toyos::BorrowedHandle<'_>) -> Result<u64, SyscallError> {
        self.claim.dma_map(region).map(|m| m.device_addr)
    }
    fn dma_unmap(&mut self, device_addr: u64) -> Result<(), SyscallError> {
        self.claim.dma_unmap(device_addr)
    }
}

struct Pending {
    conn: Connection,
    rx: FrameRx<0>,
    since: Instant,
}

/// The one `dev:pci:` claim this process was started with.
fn claim() -> PciDev {
    let label = Endowments::get()
        .labels()
        .find(|l| l.strip_prefix(DEV_PREFIX).is_some_and(|name| name.starts_with("pci:")))
        .map(str::to_string)
        .unwrap_or_else(|| panic!("usbd: started with no `{DEV_PREFIX}pci:` claim"));
    Endowments::get().take(&label).expect("usbd: the claim its label names")
}

fn main() {
    let dev = claim();
    let info = match toyos_pci_claim::describe(&dev) {
        Ok(info) => info,
        Err(why) => not_serving("usbd", &format!("the claim would not describe itself: {why}"), Link::new("usbd".into()), None),
    };
    let at = format!("{:02x}:{:02x}.{}", info.bus, info.dev, info.func);
    let host = Process { began: Instant::now(), name: format!("usbd {at}") };
    let name = host.name.clone();
    let acceptor = endow::acceptor(toyos_inspect::USB.port);
    let stop = Endowments::get().take::<toyos::wake::Bell>(toyos_quiesce::DRIVER_STOP);
    let link = Link::new(name.clone());
    // xHCI 1.2 §5.2.1: the capability registers are in BAR 0.
    let (bar, _bar) = match Bar::map(&dev, 0, info.bar_bytes[0]) {
        Ok(mapped) => mapped,
        Err(why) => not_serving(&name, &format!("its registers would not map: {why}"), link, stop),
    };
    let reset = match Reset::new(bar, &host) {
        Ok(reset) => reset,
        Err(why) => not_serving(&name, &format!("the controller cannot be driven: {why}"), link, stop),
    };
    // After the reset, so a controller refused above never had a grant, and
    // the addresses every grant and lend is placed below stated first.
    if let Err(why) = dev.dma_limit(reset.address_bits) {
        not_serving(&name, &format!("the kernel refused its {}-bit address limit: {why:?}", reset.address_bits), link, stop)
    }
    let bytes = reset.grant_bytes();
    let (grant, _region) = match Grant::alloc(&dev, bytes as u64) {
        Ok(granted) => granted,
        Err(why) => not_serving(&name, &format!("the kernel refused its grant: {why}"), link, stop),
    };
    // Halted and reset with its mastering off, and started before it is told
    // any address, which a controller may read the moment it is written.
    if let Err(why) = dev.dma_start() {
        not_serving(&name, &format!("the kernel would not start its bus mastering: {why:?}"), link, stop)
    }
    let hc = match reset.start(grant, &host) {
        Ok(hc) => hc,
        Err(why) => not_serving(&name, &format!("the controller would not run: {why}"), link, stop),
    };
    println!("{name}: running, {} ports, a {} KiB grant", hc.max_ports, bytes / 1024);
    let config = Config { input: endow::namespace().is_some(), disks: link.is_some() };
    let usb: Driver = Usb::new(hc, host, config, table::read);
    serve(&name, &dev, &usb, acceptor, link, stop)
}

/// A refusal this driver can name, before it serves anything: said once, the
/// block service told this controller is unusable, and nothing served.
fn not_serving(name: &str, why: &str, mut link: Option<Link>, stop: Option<toyos::wake::Bell>) -> ! {
    println!("{name}: NOT SERVING — {why}");
    if let Some(link) = &mut link {
        link.settle(Settled::Unusable);
    }
    idle(link, stop)
}

/// A controller that cannot be used: it keeps its link, so the service goes
/// on saying so, and serves nothing; it ends when the machine stops.
fn idle(mut link: Option<Link>, stop: Option<toyos::wake::Bell>) -> ! {
    let poller = Poller::new(2);
    loop {
        if let Some(link) = &mut link {
            link.watch(&poller, TOKEN_LINK);
        }
        if let Some(stop) = &stop {
            poller.watch(stop, READABLE, TOKEN_STOP);
        }
        let mut stopping = false;
        poller.wait(1, u64::MAX, |token| stopping |= token == TOKEN_STOP);
        if stopping {
            std::process::exit(0);
        }
        link.iter_mut().flat_map(Link::take).for_each(drop);
    }
}

/// What woke the loop, which `inspect` reads: a loop with nothing to wait
/// on wakes only for the claim or a client.
#[derive(Default)]
struct Wakes {
    /// The wait ended on its timeout.
    timed: u64,
    /// The claim read ready, and with no record behind it.
    device: u64,
    empty: u64,
    /// Interrupt records read off the claim.
    records: u64,
}

fn serve(
    name: &str,
    dev: &PciDev,
    usb: &Driver,
    acceptor: Option<toyos::port::Acceptor>,
    mut link: Option<Link>,
    stop: Option<toyos::wake::Bell>,
) -> ! {
    let mut tasks = usb.tasks();
    let mut disks: BTreeMap<u32, Sessions> = BTreeMap::new();
    let mut input = InputPort::default();
    let mut settled = false;
    let poller = Poller::new(5 + MAX_PENDING as u32 + 4 * MAX_SESSIONS as u32);
    let mut pending: Vec<Pending> = Vec::new();
    let mut ready: Vec<u64> = Vec::new();
    let mut wakes = Wakes::default();
    let mut wake_at;
    loop {
        // The driver's turn: every task run.
        wake_at = match tasks.run(&usb.wake) {
            Ok(at) => at,
            Err(Dead(why)) => ended(name, &format!("{why}; nothing drives the controller from here")),
        };
        for event in usb.disks.take_events() {
            match event {
                Event::Arrived { disk, name: disk_name, table } => {
                    let label = format!("{name} {disk_name}");
                    let sessions = Sessions::new(label.clone(), table);
                    if let Some(link) = &mut link {
                        link.offer(disk, sessions.offered());
                    }
                    disks.insert(disk, sessions);
                }
                Event::Answered { disk, token: (session, tag), write, ok, status } => {
                    disks.get_mut(&disk).expect("an answer for an offered disk").answered(Request { session, tag, write }, ok, status);
                }
                Event::Reset { disk } => {
                    let answered = disks.get_mut(&disk).expect("an offered disk").reset();
                    println!("{name}: disk {disk} came back from a reset that may have lost its cache; {answered} request(s) answered not done");
                }
                Event::Gone { disk } => {
                    if let Some(link) = &mut link {
                        link.gone(disk);
                    }
                    if let Some(mut sessions) = disks.remove(&disk) {
                        sessions.end(&mut Disk { usb, disk, claim: dev });
                    }
                }
            }
        }
        if !settled && usb.settled() {
            settled = true;
            println!("{name}: settled, {} device(s) named, {} disk(s) offered", usb.named.borrow().len(), disks.len());
            if let Some(link) = &mut link {
                link.settle(Settled::Offered);
            }
        }
        for handed in link.iter_mut().flat_map(Link::take) {
            let on = match &handed {
                Handed::Session { session, .. } => session.disk,
                Handed::Describe { disk, .. } => *disk,
                Handed::Designate { order, .. } => order.disk,
            };
            let Some(sessions) = disks.get_mut(&on) else {
                println!("{name}: the block service handed work on disk {on}, which is not served");
                continue;
            };
            let mut port = Disk { usb, disk: on, claim: dev };
            match handed {
                Handed::Session { session, conn, region } => sessions.open(&mut port, conn, region, session.unique, session.writes),
                Handed::Describe { conn, .. } => sessions.describe(conn),
                Handed::Designate { order, conn } => sessions.designate(&mut port, conn, order),
            }
        }
        let submitted = usb.disks.submitted();
        for (&disk, sessions) in disks.iter_mut() {
            for id in sessions.ids() {
                if ready.contains(&(TOKEN_SESSION | u64::from(disk) << 16 | id)) {
                    sessions.hear(id);
                }
            }
            let mut port = Disk { usb, disk, claim: dev };
            sessions.pull(&mut port);
            sessions.publish();
            sessions.retire(&mut port);
        }
        usb.input.borrow_mut().flush(&mut input, usb.wake.now());

        // inspect's clients.
        if let (true, Some(acceptor)) = (ready.contains(&TOKEN_ACCEPT), &acceptor) {
            match acceptor.accept() {
                Ok(conn) if pending.len() < MAX_PENDING => pending.push(Pending { conn, rx: FrameRx::new(), since: Instant::now() }),
                Ok(conn) => println!("{name}: refusing client {} — {MAX_PENDING} are already waiting", conn.as_handle().raw().0),
                Err(why) => println!("{name}: the kernel refused a client's connection: {why:?}"),
            }
        }
        pending.retain_mut(|p| {
            if p.since.elapsed() >= HANDSHAKE_TIMEOUT {
                return false;
            }
            if !ready.contains(&(TOKEN_PENDING + u64::from(p.conn.as_handle().raw().0))) {
                return true;
            }
            match p.rx.pump(&p.conn) {
                RxStep::Idle => true,
                RxStep::Frame { msg_type: MSG_INSPECT, .. } => {
                    answer(&p.conn, usb, &wakes, wake_at);
                    false
                }
                _ => false,
            }
        });

        // Work a session queued is the driver's to run before anything waits.
        if usb.disks.submitted() != submitted {
            ready.clear();
            continue;
        }
        poller.watch(dev, READABLE, TOKEN_IRQ);
        if let Some(stop) = &stop {
            poller.watch(stop, READABLE, TOKEN_STOP);
        }
        if let Some(acceptor) = &acceptor {
            poller.watch(acceptor, READABLE, TOKEN_ACCEPT);
        }
        if let Some(link) = &link {
            link.watch(&poller, TOKEN_LINK);
        }
        if let Some(conn) = &input.conn {
            if usb.input.borrow().full() {
                poller.watch(conn, WRITABLE, TOKEN_INPUT);
            }
        }
        for (&disk, sessions) in &disks {
            sessions.watch(&poller, TOKEN_SESSION | u64::from(disk) << 16);
        }
        for p in &pending {
            poller.watch(&p.conn, READABLE, TOKEN_PENDING + u64::from(p.conn.as_handle().raw().0));
        }
        let now = usb.wake.now();
        let mut timeout = wake_at.map_or(u64::MAX, |at| at.saturating_sub(now));
        if let Some(first) = pending.iter().map(|p| p.since).min() {
            timeout = timeout.min(HANDSHAKE_TIMEOUT.saturating_sub(first.elapsed()).as_nanos() as u64);
        }
        ready.clear();
        poller.wait(1, timeout, |token| ready.push(token));
        if ready.is_empty() {
            wakes.timed += 1;
        }
        if ready.contains(&TOKEN_STOP) {
            end_for_the_stop(name, usb, tasks);
        }
        // **The record is taken, not merely noticed**: a claim reads ready
        // while it holds one, so one left would wake every wait after it.
        if ready.contains(&TOKEN_IRQ) {
            wakes.device += 1;
            match dev.irq() {
                Ok(_) => {
                    wakes.records += 1;
                    while usb.hc.take_events() && usb.hc.event_waiting() {}
                }
                Err(SyscallError::WouldBlock) => wakes.empty += 1,
                Err(why) => ended(name, &format!("the claim refused its interrupt record ({why:?}); nothing drives it from here")),
            }
        }
    }
}

/// The supervisor says the machine stops, every file server synced: every
/// device handed back with a port reset and the controller left reset
/// (`Hc::stop`), and then this process ends, which releases the claim. What
/// a task still held is let go of: the driver ends here.
fn end_for_the_stop<'u>(name: &str, usb: &'u Driver, tasks: Tasks<'u>) -> ! {
    usb.hc.ending.set(true);
    drop(tasks);
    let stopped = std::cell::Cell::new(None);
    let mut stop = Tasks::default();
    stop.spawn(async {
        stopped.set(Some(usb.hc.stop(&usb.wake).await));
        Ok(())
    });
    while !stop.is_empty() {
        match stop.run(&usb.wake) {
            Ok(Some(at)) => std::thread::sleep(Duration::from_nanos(at.saturating_sub(usb.wake.now()))),
            Ok(None) => {}
            Err(Dead(why)) => ended(name, &format!("{why}, in the stop")),
        }
    }
    let stopped = stopped.get().expect("the stop's task set it before it ended");
    println!("{name}: the machine stops: {stopped}");
    // **The exit code is the account that is sure to be read**: the kernel
    // records it, and this line may reach no log, whose volume this process
    // just took down.
    std::process::exit(if stopped.whole() { 0 } else { STOP_UNFINISHED })
}

/// The controller stopped answering, or the claim refused it, while usbd
/// served it: said by name, and this process ends, which takes every device
/// on the controller with it and releases the claim, so the supervisor may
/// drive the function again.
fn ended(name: &str, why: &str) -> ! {
    println!("{name}: ENDED — {why}");
    std::process::exit(CONTROLLER_LOST)
}

/// What usbd ends with when the controller it served is lost ([`ended`]).
const CONTROLLER_LOST: i32 = 2;

/// What usbd ends with when its stop left a port in reset or the controller
/// running: [`toyos_xhci::hc::Stopped::whole`] false.
const STOP_UNFINISHED: i32 = 3;

fn answer(conn: &Connection, usb: &Driver, wakes: &Wakes, wake_at: Option<Nanos>) {
    let mut snap = Snapshot::new(toyos_inspect::USB);
    snap.put("ports", u32::from(usb.hc.max_ports));
    let named = usb.named.borrow();
    snap.put("devices", named.len());
    for d in named.values() {
        let at = format!("device.{}", d.port + 1);
        snap.put(&format!("{at}.id"), format!("{:04x}:{:04x}", d.device.vendor, d.device.product));
        snap.put(&format!("{at}.speed"), usb::speed_name(d.speed));
        snap.put(&format!("{at}.slot"), u32::from(d.slot));
        snap.put(&format!("{at}.functions"), d.functions.join("; "));
    }
    snap.put("noop", match usb.noop.get() {
        None => "outstanding",
        Some(true) => "completed",
        Some(false) => "unanswered",
    });
    let hc = usb.hc.counts.get();
    snap.put("interrupts.records", wakes.records);
    snap.put("events.taken", hc.events);
    snap.put("events.unannounced", hc.unannounced);
    snap.put("events.stray", hc.stray);
    snap.put("events.other", hc.other);
    snap.put("operations.silent", hc.silent);
    snap.put("commands.aborted", hc.aborted);
    snap.put("wakes.timed", wakes.timed);
    snap.put("wakes.device", wakes.device);
    snap.put("wakes.empty", wakes.empty);
    let now = usb.wake.now();
    snap.put("deadline_ns", wake_at.map_or("none".to_string(), |at| at.saturating_sub(now).to_string()));
    let hid = usb.hid.get();
    snap.put("hid.reserved", hid.reserved);
    snap.put("hid.unlisted", hid.unlisted);
    snap.put("hid.unsent_button", hid.unsent_button);
    snap.put("hid.unbound", hid.unbound);
    let input = usb.input.borrow().counts;
    snap.put("input.sent", input.sent);
    snap.put("input.coalesced", input.coalesced);
    snap.put("input.unowned", input.unowned);
    snap.put("input.opened", input.opened);
    let encoded = snap.encode().unwrap_or_else(|why| panic!("usbd: its snapshot: {why}"));
    if let Err(why) = conn.try_send_bytes(MSG_SNAPSHOT, &encoded) {
        println!("usbd: dropping client {} — its answer would not go: {why:?}", conn.as_handle().raw().0);
    }
}
