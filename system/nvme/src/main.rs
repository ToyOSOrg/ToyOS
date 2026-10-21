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
use toyos_blockserve::link::Link;
use toyos_blockserve::sessions::MAX_SESSIONS;
use toyos_blockserve::{Device, Sessions};

mod nvme;

use nvme::{Controller, Done, Owner};

/// How long a command may go unanswered before the controller is reset to
/// take it back. Policy: NVMe defines no per-command bound, and a flush of a
/// large cache is the slowest thing a healthy drive does.
const COMMAND_SILENCE: Duration = Duration::from_secs(10);

/// The one disk a controller's first namespace is.
const DISK: u32 = 0;

const TOKEN_IRQ: u64 = 0;
const TOKEN_LINK: u64 = 1;
const TOKEN_SESSION: u64 = 0x1_0000;

/// The controller as `toyos_gpt` reads it: its own sectors, served a 4 KiB
/// block at a time, and floored to whole blocks.
struct Sectors<'a> {
    ctrl: &'a mut Controller,
    block: Vec<u8>,
}

impl toyos_gpt::Sectors for Sectors<'_> {
    fn lba_bytes(&self) -> u32 {
        self.ctrl.lba_bytes
    }
    fn lba_count(&self) -> u64 {
        let per = BLOCK_BYTES as u64 / self.ctrl.lba_bytes as u64;
        self.ctrl.sectors / per * per
    }
    fn lba_count_granularity(&self) -> core::num::NonZeroU64 {
        core::num::NonZeroU64::new(BLOCK_BYTES as u64 / self.ctrl.lba_bytes as u64).expect("at least one")
    }
    fn read_lba(&mut self, lba: u64, buf: &mut [u8]) -> bool {
        let lba_bytes = self.ctrl.lba_bytes as u64;
        let at = lba * lba_bytes;
        if !self.ctrl.read_block(at / BLOCK_BYTES as u64, &mut self.block) {
            return false;
        }
        let off = (at % BLOCK_BYTES as u64) as usize;
        buf.copy_from_slice(&self.block[off..off + lba_bytes as usize]);
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
    fn dma_map(&mut self, region: toyos::RawHandle) -> Result<u64, SyscallError> {
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
    let mut link = Link::new(name.clone());
    let mut ctrl = match Controller::open(dev, &info) {
        Ok(ctrl) => ctrl,
        Err(why) => {
            println!("{name}: NOT SERVING — {why}");
            link.settle(Settled::Unusable);
            idle(&mut link);
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
    let parts = toyos_blockserve::table::read(&name, &mut Sectors { ctrl: &mut ctrl, block: vec![0u8; BLOCK_BYTES] });
    let mut disk = Sessions::new(name.clone(), parts);
    link.offer(DISK, disk.offered());
    link.settle(Settled::Offered);
    serve(&name, &mut ctrl, &mut disk, &mut link)
}

/// A driver whose controller cannot be used: it keeps its link, so the
/// service goes on saying so, and opens nothing.
fn idle(link: &mut Link) -> ! {
    let poller = Poller::new(1);
    loop {
        link.watch(&poller, TOKEN_LINK);
        poller.wait(1, u64::MAX, |_| {});
        for handed in link.take() {
            toyos_abi::syscall::close(handed.region);
            drop(handed.conn);
        }
    }
}

fn serve(name: &str, ctrl: &mut Controller, disk: &mut Sessions, link: &mut Link) -> ! {
    let poller = Poller::new(2 + MAX_SESSIONS as u32);
    let mut ready: Vec<u64> = Vec::new();
    let mut done: Vec<Done> = Vec::new();
    loop {
        poller.watch(ctrl.claim(), toyos::poller::READABLE, TOKEN_IRQ);
        link.watch(&poller, TOKEN_LINK);
        disk.watch(&poller, TOKEN_SESSION);
        let timeout = ctrl
            .oldest()
            .map_or(u64::MAX, |at| COMMAND_SILENCE.saturating_sub(at.elapsed()).as_nanos() as u64);
        ready.clear();
        poller.wait(1, timeout, |token| ready.push(token));

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
            if handed.session.disk != DISK {
                println!("{name}: the block service handed a session on disk {}, which this driver never offered", handed.session.disk);
                toyos_abi::syscall::close(handed.region);
                continue;
            }
            disk.open(ctrl, handed.conn, handed.region, handed.session.unique, handed.session.writes);
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
