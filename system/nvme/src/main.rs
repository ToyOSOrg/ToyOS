//! nvme: one NVMe controller, driven from userland, its one namespace offered
//! to the block service as a disk.
//!
//! **It holds a claim on one PCI function and a connector to the block
//! service's `disk` port, and nothing else of the machine.** The supervisor
//! starts one per function its `drives` row names; the kernel keeps config
//! space, the interrupt vector and the function's address space, and this
//! driver never names an address the kernel did not hand it ([`nvme`]).
//!
//! **It reads its own partition table** and offers the disk with every
//! partition it names; the service hands it each session opened on one,
//! which it serves itself out of the client's region (`toyos_blockserve`).
//! A controller it cannot use is said once and offered as such, so a listing
//! says the disk failed rather than that there is none, and it serves
//! nothing.

use std::time::{Duration, Instant};

use toyos::endow::Endowments;
use toyos::poller::Poller;
use toyos::PciDev;
use toyos_abi::syscall::{SyscallError, DEV_PREFIX};
use toyos_blockring::disk::Settled;
use toyos_blockring::BLOCK_BYTES;
use toyos_blockserve::link::{Handed, Link};
use toyos_blockserve::sessions::MAX_SESSIONS;
use toyos_blockserve::{Device, Sessions};

mod nvme;

use nvme::{Controller, Done, Owner};

/// How long a command may go unanswered before the controller is reset to
/// take it back. Policy: NVMe defines no per-command bound, and a flush of a
/// large cache is the slowest thing a healthy drive does.
const COMMAND_SILENCE: Duration = Duration::from_secs(10);

/// How long the stop's normal shutdown is given: a policy, below the
/// supervisor's wait for every disk driver (`toyos_quiesce::DRIVERS_MS`).
const SHUTDOWN_BOUND: Duration = Duration::from_secs(2);

/// The one disk a controller's first namespace is.
const DISK: u32 = 0;

const TOKEN_IRQ: u64 = 0;
const TOKEN_LINK: u64 = 1;
const TOKEN_STOP: u64 = 2;
const TOKEN_SESSION: u64 = 0x1_0000;

/// The controller as `toyos_gpt` reads it: every sector of the namespace,
/// a 4 KiB block at a time where the block is whole and one sector at a time
/// past the last whole block, where the backup table is on a namespace whose
/// count is not whole blocks.
struct Sectors<'a> {
    ctrl: &'a mut Controller,
    block: Vec<u8>,
}

impl toyos_gpt::Sectors for Sectors<'_> {
    fn lba_bytes(&self) -> u32 {
        self.ctrl.lba_bytes
    }
    fn lba_count(&self) -> u64 {
        self.ctrl.sectors
    }
    fn read_lba(&mut self, lba: u64, buf: &mut [u8]) -> bool {
        let per = BLOCK_BYTES as u64 / self.ctrl.lba_bytes as u64;
        let first = lba / per * per;
        if first + per > self.ctrl.sectors {
            return self.ctrl.read_sectors(lba, buf);
        }
        if !self.ctrl.read_sectors(first, &mut self.block) {
            return false;
        }
        let off = ((lba - first) * self.ctrl.lba_bytes as u64) as usize;
        buf.copy_from_slice(&self.block[off..off + self.ctrl.lba_bytes as usize]);
        true
    }
}

impl Device for Controller {
    fn has_room(&self) -> bool {
        Controller::has_room(self)
    }
    fn submit_io(&mut self, write: bool, block: u64, blocks: u32, at: u64, owner: toyos_blockserve::Owner) {
        Controller::submit_io(self, write, block, blocks, at, Owner::Session(owner));
    }
    fn submit_flush(&mut self, owner: toyos_blockserve::Owner) -> bool {
        // No volatile cache: every write answered is on the medium already.
        if self.vwc {
            Controller::submit_flush(self, Owner::Session(owner));
        }
        self.vwc
    }
    fn dma_map(&mut self, region: toyos::BorrowedHandle<'_>) -> Result<u64, SyscallError> {
        self.claim().dma_map(region).map(|m| m.device_addr)
    }
    fn dma_unmap(&mut self, device_addr: u64) -> Result<(), SyscallError> {
        self.claim().dma_unmap(device_addr)
    }
}

/// The one `dev:pci:` claim this process was started with.
fn claim() -> PciDev {
    let label = Endowments::get()
        .labels()
        .find(|l| l.strip_prefix(DEV_PREFIX).is_some_and(|name| name.starts_with("pci:")))
        .map(str::to_string)
        .unwrap_or_else(|| panic!("nvme: started with no `{DEV_PREFIX}pci:` claim"));
    Endowments::get().take(&label).expect("nvme: the claim its label names")
}

fn main() {
    let dev = claim();
    let info = dev.describe().unwrap_or_else(|why| panic!("nvme: the claim would not describe itself: {why:?}"));
    let name = format!("nvme {:02x}:{:02x}.{}", info.bus, info.dev, info.func);
    let stop = Endowments::get().take::<toyos::wake::Bell>(toyos_quiesce::DRIVER_STOP);
    let mut link = Link::new(name.clone())
        .unwrap_or_else(|| panic!("{name}: started with no `{}` port: a disk driver's row receives it", toyos_blockring::disk::PORT));
    let mut ctrl = match Controller::open(dev, &info) {
        Ok(ctrl) => ctrl,
        Err(why) => {
            println!("{name}: NOT SERVING — {why}");
            link.settle(Settled::Unusable);
            idle(&mut link, stop.as_ref());
        }
    };
    println!(
        "{name}: up: {} I/O queues of {} commands, volatile write cache {}, {}-byte sectors, {} sectors",
        ctrl.queues(),
        nvme::COMMANDS_PER_QUEUE,
        if ctrl.vwc { "present, so a flush issues Flush" } else { "absent, so a flush issues nothing" },
        ctrl.lba_bytes,
        ctrl.sectors
    );
    say_power(&name, &ctrl.power);
    let read = toyos_blockserve::table::read(&mut Sectors { ctrl: &mut ctrl, block: vec![0u8; BLOCK_BYTES] });
    let mut disk = Sessions::new(name.clone(), read);
    link.offer(DISK, disk.offered());
    link.settle(Settled::Offered);
    serve(&name, &mut ctrl, &mut disk, &mut link, stop.as_ref())
}

/// The controller's power states and the APST table set over them, one line
/// each: what a bench's idle power and its first I/O after idle are read
/// against.
fn say_power(name: &str, power: &nvme::Power) {
    match power {
        nvme::Power::Unsupported => println!("{name}: no autonomous power state transitions: the controller supports none"),
        nvme::Power::Refused(status) => {
            println!("{name}: no autonomous power state transitions: the controller refused the table (status {status:#x})")
        }
        nvme::Power::Autonomous { states, plan } => {
            for (n, s) in states.iter().enumerate() {
                let watts = |p: Option<u64>| p.map_or("unreported".to_string(), |p| format!("{}.{:04} W", p / 10_000, p % 10_000));
                println!(
                    "{name}: power state {n}: {}, entry {} us, exit {} us, max {}, idle {}",
                    if s.non_operational { "non-operational" } else { "operational" },
                    s.entry_us,
                    s.exit_us,
                    watts(s.max_power),
                    watts(s.idle_power),
                );
            }
            let steps: Vec<String> =
                plan.iter().enumerate().filter_map(|(n, t)| t.map(|t| format!("{n}->{} after {} ms", t.to, t.after_ms))).collect();
            println!(
                "{name}: autonomous power state transitions on, round trips within {} us: {}",
                nvme_apst::EXIT_BOUND_US,
                if steps.is_empty() { "no state admitted".to_string() } else { steps.join(", ") }
            );
        }
    }
}

/// A driver whose controller cannot be used: it keeps its link, so the
/// service goes on saying so, and opens nothing.
fn idle(link: &mut Link, stop: Option<&toyos::wake::Bell>) -> ! {
    let poller = Poller::new(2);
    loop {
        link.watch(&poller, TOKEN_LINK);
        if let Some(stop) = stop {
            poller.watch(stop, toyos::poller::READABLE, TOKEN_STOP);
        }
        let mut stopping = false;
        poller.wait(1, u64::MAX, |token| stopping |= token == TOKEN_STOP);
        if stopping {
            std::process::exit(0);
        }
        drop(link.take());
    }
}

fn serve(name: &str, ctrl: &mut Controller, disk: &mut Sessions, link: &mut Link, stop: Option<&toyos::wake::Bell>) -> ! {
    let poller = Poller::new(3 + MAX_SESSIONS as u32);
    let mut ready: Vec<u64> = Vec::new();
    let mut done: Vec<Done> = Vec::new();
    loop {
        poller.watch(ctrl.claim(), toyos::poller::READABLE, TOKEN_IRQ);
        link.watch(&poller, TOKEN_LINK);
        disk.watch(&poller, TOKEN_SESSION);
        if let Some(stop) = stop {
            poller.watch(stop, toyos::poller::READABLE, TOKEN_STOP);
        }
        let timeout = ctrl
            .oldest()
            .map_or(u64::MAX, |at| COMMAND_SILENCE.saturating_sub(at.elapsed()).as_nanos() as u64);
        ready.clear();
        poller.wait(1, timeout, |token| ready.push(token));
        // The supervisor says the machine stops, every file server synced.
        if ready.contains(&TOKEN_STOP) {
            let done = ctrl.shut_down(SHUTDOWN_BOUND);
            println!("{name}: the machine stops: shutdown {}", if done { "complete" } else { "not complete in its bound" });
            std::process::exit(0);
        }

        if let Err(why) = ctrl.take_interrupt() {
            panic!(
                "{name}: the claim refused its interrupt read ({why:?}): the unit refused this controller an \
                 access, and the claim answers nothing now"
            );
        }
        ctrl.reap(&mut done);
        for d in done.drain(..) {
            answer(name, disk, d);
        }
        if ctrl.oldest().is_some_and(|at: Instant| at.elapsed() >= COMMAND_SILENCE) {
            reset(name, ctrl, disk);
        }

        for handed in link.take() {
            let on = match &handed {
                Handed::Session { session, .. } => session.disk,
                Handed::Describe { disk, .. } => *disk,
                Handed::Designate { order, .. } => order.disk,
            };
            if on != DISK {
                println!("{name}: the block service handed work on disk {on}, which this driver never offered");
                continue;
            }
            match handed {
                Handed::Session { session, conn, region } => disk.open(ctrl, conn, region, session.unique, session.writes),
                Handed::Describe { conn, .. } => disk.describe(conn),
                Handed::Designate { order, conn } => disk.designate(ctrl, conn, order),
            }
        }
        for id in disk.ids() {
            if ready.contains(&(TOKEN_SESSION + id)) {
                disk.hear(id);
            }
        }
        disk.pull(ctrl);
        disk.publish();
        if disk.retire(ctrl) > 0 {
            println!(
                "{name}: the device has held at most {} commands at once, issued per queue {}",
                ctrl.peak,
                ctrl.spread()
            );
        }
    }
}

fn answer(name: &str, disk: &mut Sessions, done: Done) {
    match done.owner {
        Owner::Session(owner) => disk.answered(owner, done.ok, u32::from(done.status)),
        Owner::Driver => panic!("{name}: the device answered a command of this driver's own while serving"),
    }
}

/// Reset the controller under every command it holds.
fn reset(name: &str, ctrl: &mut Controller, disk: &mut Sessions) {
    println!("{name}: a command went unanswered for {COMMAND_SILENCE:?}; resetting the controller");
    let mut done = Vec::new();
    let aborted = ctrl
        .reset(&mut done)
        .unwrap_or_else(|why| panic!("{name}: the controller did not come back from its reset: {why}"));
    for d in done {
        answer(name, disk, d);
    }
    let answered = disk.reset();
    println!(
        "{name}: controller reset; {} commands it held were answered not done ({answered} of them sessions'), \
         and the loss count is {}",
        aborted.len(),
        disk.losses()
    );
}
