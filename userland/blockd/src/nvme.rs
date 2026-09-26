//! The NVMe controller blockd drives, NVMe Base Specification 2.0 throughout:
//! §3.1.4 for the registers, §3.3 for the queues, §3.5.1 for bring-up, §5 for
//! the admin commands, and the NVM Command Set 1.0 for read, write and flush.
//!
//! **Several I/O queue pairs, each holding many commands at once.** A command
//! is a slot of its queue, named by its command identifier; every completion
//! queue interrupts on the claim's one MSI-X vector, and [`Controller::reap`]
//! walks all of them. What the device writes into a completion entry is not
//! trusted: an identifier naming no slot that is waiting is dropped and said
//! once, never acted on.
//!
//! **Every address a command carries is one the claim's domain maps**: this
//! driver's own grant for the queues and the PRP lists, or a session region
//! the kernel mapped for it (`SYS_DEVICE_DMA_MAP`). An address outside both is
//! refused at the unit and recorded against the claim, and the claim answers
//! nothing afterwards — which ends this process.
//!
//! **A flush issues Flush when the controller has a volatile write cache**
//! (Identify Controller `VWC` bit 0, §5.17.2.1): a write's completion says
//! the controller took the data, not that it is on the medium. With no
//! volatile cache a completed write is on the medium, and a flush has nothing
//! to issue.
//!
//! **A command the controller does not answer is reclaimed by a controller
//! reset** (§3.7.2): `CC.EN` cleared, every command it held is gone, the queues
//! are made again. Whoever owned one is told it was not done.

use std::time::{Duration, Instant};

use crate::window::Window;
use toyos::poller::{Poller, READABLE};
use toyos::shm::SharedMemory;
use toyos::{DmaRegion, PciDev};
use toyos_abi::syscall::SyscallError;

const REG_CAP: usize = 0x00;
const REG_CC: usize = 0x14;
const REG_CSTS: usize = 0x1C;
const REG_AQA: usize = 0x24;
const REG_ASQ: usize = 0x28;
const REG_ACQ: usize = 0x30;
const DOORBELLS: usize = 0x1000;

/// `CC.EN`, with `IOSQES` = 6 and `IOCQES` = 4 (64- and 16-byte entries),
/// `MPS` = 0 (4 KiB pages) and the NVM command set.
const CC_ENABLED: u32 = 1 | (6 << 16) | (4 << 20);
const CSTS_RDY: u32 = 1;
const CSTS_CFS: u32 = 1 << 1;

const SQE_BYTES: usize = 64;
const CQE_BYTES: usize = 16;
const PAGE: usize = 4096;

const ADMIN_DEPTH: u16 = 32;
/// Entries per I/O queue; a queue holds one fewer command than it has entries.
const IO_DEPTH: u16 = 64;
/// The most I/O queue pairs this driver asks for.
const MAX_IO_QUEUES: u16 = 4;

const ADMIN_CREATE_IO_SQ: u8 = 0x01;
const ADMIN_CREATE_IO_CQ: u8 = 0x05;
const ADMIN_IDENTIFY: u8 = 0x06;
const ADMIN_SET_FEATURES: u8 = 0x09;
const FEATURE_NUMBER_OF_QUEUES: u32 = 0x07;
const IO_FLUSH: u8 = 0x00;
const IO_WRITE: u8 = 0x01;
const IO_READ: u8 = 0x02;

/// Where everything this driver itself owns is in its one grant.
const OFF_ADMIN_SQ: usize = 0x0000;
const OFF_ADMIN_CQ: usize = 0x1000;
const OFF_IDENTIFY: usize = 0x2000;
const OFF_SCRATCH: usize = 0x3000;
/// Queue `q`'s submission page, and its completion page after it.
const OFF_IO: usize = 0x4000;
/// One PRP list page per command slot of every I/O queue.
const OFF_PRP: usize = 0x10000;
const GRANT_BYTES: u64 = 2 * 1024 * 1024;

const _: () = assert!(OFF_IO + MAX_IO_QUEUES as usize * 2 * PAGE <= OFF_PRP);
const _: () = assert!(OFF_PRP + MAX_IO_QUEUES as usize * IO_DEPTH as usize * PAGE <= GRANT_BYTES as usize);
const _: () = assert!(ADMIN_DEPTH as usize * SQE_BYTES <= PAGE);
const _: () = assert!(IO_DEPTH as usize * SQE_BYTES <= PAGE);

/// Whose a command is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    /// This driver's own, waited for in place.
    Driver,
    /// A session's request `tag`, and whether it writes.
    Session { session: u64, tag: u32, write: bool },
}

/// One command's answer.
#[derive(Clone, Copy, Debug)]
pub struct Done {
    pub owner: Owner,
    pub ok: bool,
    pub status: u16,
    dw0: u32,
}

/// Why the controller could not be brought up, or went on. Carried, so the one
/// line that says so says which.
#[derive(Debug)]
pub enum Refusal {
    Kernel(&'static str, SyscallError),
    NoRegisters,
    NotNvme(u32),
    Fatal(&'static str),
    Silent(&'static str, Duration),
    Admin(&'static str, u16),
    Geometry(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Kernel(what, why) => write!(f, "the kernel refused {what}: {why:?}"),
            Self::NoRegisters => {
                write!(f, "BAR 0, where NVMe keeps its registers and doorbells, is not one this claim maps whole")
            }
            Self::NotNvme(class) => write!(f, "class code {class:#08x} is not an NVM Express controller"),
            Self::Fatal(when) => write!(f, "the controller reported a fatal status {when}"),
            Self::Silent(what, after) => write!(f, "{what} was not answered in {after:?}"),
            Self::Admin(what, status) => write!(f, "{what} failed with status {status:#x}"),
            Self::Geometry(why) => write!(f, "{why}"),
        }
    }
}

/// One submission entry's fields (§4.2); the identifier is the slot's.
#[derive(Clone, Copy, Default)]
struct Command {
    opcode: u8,
    nsid: u32,
    prp1: u64,
    prp2: u64,
    cdw10: u32,
    cdw11: u32,
    cdw12: u32,
}

impl Command {
    fn write(&self, entry: Window, cid: u16) {
        entry.zero();
        entry.write::<u32>(0, self.opcode as u32 | ((cid as u32) << 16));
        entry.write::<u32>(4, self.nsid);
        entry.write::<u64>(24, self.prp1);
        entry.write::<u64>(32, self.prp2);
        entry.write::<u32>(40, self.cdw10);
        entry.write::<u32>(44, self.cdw11);
        entry.write::<u32>(48, self.cdw12);
    }
}

#[derive(Clone, Copy)]
struct Slot {
    owner: Owner,
    since: Instant,
}

struct Queue {
    qid: u16,
    depth: u16,
    sq: usize,
    cq: usize,
    tail: u16,
    head: u16,
    phase: bool,
    slots: Vec<Option<Slot>>,
    free: Vec<u16>,
}

impl Queue {
    fn new(qid: u16, depth: u16, sq: usize, cq: usize) -> Self {
        // One fewer than the entries: a full submission ring would read empty.
        let free = (0..depth - 1).rev().collect();
        Self { qid, depth, sq, cq, tail: 0, head: 0, phase: true, slots: vec![None; depth as usize], free }
    }

    fn busy(&self) -> usize {
        (self.depth - 1) as usize - self.free.len()
    }
}

pub struct Controller {
    dev: PciDev,
    _bar: SharedMemory,
    regs: Window,
    _grant: DmaRegion,
    dma: Window,
    dma_addr: u64,
    stride: usize,
    /// `CAP.TO`: the controller's own worst case for a `CSTS.RDY` transition.
    ready_bound: Duration,
    admin: Queue,
    io: Vec<Queue>,
    pub lba_bytes: u32,
    pub sectors: u64,
    pub vwc: bool,
    /// The most 4 KiB blocks one command may move (`MDTS`).
    pub max_blocks: u32,
    poller: Poller,
    /// A device answer naming no waiting slot, said once.
    stray: bool,
    /// The most commands the device has held at once.
    pub peak: usize,
    /// Commands issued down each I/O queue.
    pub issued: Vec<u64>,
    /// `--silence-write <n>`: the answer to the `n`th write a session asked
    /// for, from 1, is not given, as if the device never gave it.
    silence: Option<u32>,
}

impl Controller {
    /// Bring the controller up (§3.5.1): disable, program the admin queue,
    /// enable, identify, and make the I/O queue pairs.
    pub fn open(dev: PciDev, silence: Option<u32>) -> Result<Self, Refusal> {
        let class = dev
            .config_read(0x08, toyos_abi::syscall::RegWidth::U32)
            .map_err(|e| Refusal::Kernel("the class code", e))?;
        if class >> 8 != 0x01_08_02 {
            return Err(Refusal::NotNvme(class >> 8));
        }
        let info = dev.describe().map_err(|e| Refusal::Kernel("the claim's description", e))?;
        let bar_bytes = info.bar_bytes[0];
        if bar_bytes <= DOORBELLS as u64 {
            return Err(Refusal::NoRegisters);
        }
        let bar = dev.map_bar(0, bar_bytes).map_err(|e| Refusal::Kernel("the register window", e))?;
        // SAFETY: the mapping is `bar_bytes` long and lives as long as `bar`,
        // which this struct holds.
        let regs = unsafe { Window::new(bar.as_ptr(), bar_bytes as usize) };
        let cap: u64 = regs.read(REG_CAP);
        let stride = 4usize << ((cap >> 32) & 0xF);
        if DOORBELLS + (2 * MAX_IO_QUEUES as usize + 2) * stride > bar_bytes as usize {
            return Err(Refusal::NoRegisters);
        }
        let mqes = (cap & 0xFFFF) as u32 + 1;
        if mqes < IO_DEPTH as u32 {
            return Err(Refusal::Geometry(format!("the controller's queues hold {mqes} entries")));
        }
        let grant = dev.dma_alloc(GRANT_BYTES).map_err(|e| Refusal::Kernel("a DMA grant", e))?;
        // SAFETY: the grant is at least `GRANT_BYTES` and lives as long as
        // `grant`, which this struct holds.
        let dma = unsafe { Window::new(grant.memory.as_ptr(), GRANT_BYTES as usize) };
        dma.zero();
        let dma_addr = grant.device_addr;
        let to = ((cap >> 24) & 0xFF).max(1);
        let mut ctrl = Self {
            dev,
            _bar: bar,
            regs,
            _grant: grant,
            dma,
            dma_addr,
            stride,
            ready_bound: Duration::from_millis(to * 500),
            admin: Queue::new(0, ADMIN_DEPTH, OFF_ADMIN_SQ, OFF_ADMIN_CQ),
            io: Vec::new(),
            lba_bytes: 0,
            sectors: 0,
            vwc: false,
            max_blocks: 0,
            poller: Poller::new(1),
            stray: false,
            peak: 0,
            issued: Vec::new(),
            silence,
        };
        ctrl.enable()?;
        ctrl.identify()?;
        ctrl.make_io_queues()?;
        Ok(ctrl)
    }

    pub fn claim(&self) -> &PciDev {
        &self.dev
    }

    fn csts(&self) -> u32 {
        self.regs.read(REG_CSTS)
    }

    /// Wait for `CSTS.RDY` to read `ready`.
    ///
    /// **Polled, and bounded by `CAP.TO`**: the register raises no interrupt
    /// and `CAP.TO` is the controller's own declared bound on the transition
    /// (§3.1.4.1, §3.5.1).
    fn settle(&self, ready: bool, what: &'static str) -> Result<(), Refusal> {
        let start = Instant::now();
        while (self.csts() & CSTS_RDY != 0) != ready {
            if self.csts() & CSTS_CFS != 0 {
                return Err(Refusal::Fatal(what));
            }
            if start.elapsed() > self.ready_bound {
                return Err(Refusal::Silent(what, self.ready_bound));
            }
            std::thread::yield_now();
        }
        Ok(())
    }

    /// Disable, program the admin queue, and enable.
    fn enable(&mut self) -> Result<(), Refusal> {
        let cc: u32 = self.regs.read(REG_CC);
        if cc & 1 != 0 {
            self.regs.write::<u32>(REG_CC, cc & !1);
        }
        self.settle(false, "CSTS.RDY clearing")?;
        self.dma.sub(OFF_ADMIN_SQ, PAGE).zero();
        self.dma.sub(OFF_ADMIN_CQ, PAGE).zero();
        self.admin = Queue::new(0, ADMIN_DEPTH, OFF_ADMIN_SQ, OFF_ADMIN_CQ);
        let aqa = ((ADMIN_DEPTH as u32 - 1) << 16) | (ADMIN_DEPTH as u32 - 1);
        self.regs.write::<u32>(REG_AQA, aqa);
        self.regs.write::<u64>(REG_ASQ, self.dma_addr + OFF_ADMIN_SQ as u64);
        self.regs.write::<u64>(REG_ACQ, self.dma_addr + OFF_ADMIN_CQ as u64);
        self.regs.write::<u32>(REG_CC, CC_ENABLED);
        self.settle(true, "CSTS.RDY setting")
    }

    fn identify(&mut self) -> Result<(), Refusal> {
        let buf = self.dma_addr + OFF_IDENTIFY as u64;
        let id = self.dma.sub(OFF_IDENTIFY, PAGE);
        id.zero();
        self.admin_command("Identify Controller", Command { opcode: ADMIN_IDENTIFY, prp1: buf, cdw10: 1, ..Command::default() })?;
        let mdts: u8 = id.read(77);
        let vwc: u8 = id.read(525);
        self.vwc = vwc & 1 != 0;
        // A power of two in units of the minimum page, 4 KiB here; zero is no
        // limit.
        self.max_blocks = match mdts {
            0 => u32::MAX,
            n if n >= 20 => u32::MAX,
            n => 1u32 << n,
        };
        if self.max_blocks < toyos_blockring::MAX_REQUEST_BLOCKS {
            return Err(Refusal::Geometry(format!(
                "MDTS allows {} blocks per command and one request may be {}, and this driver does \
                 not split a request",
                self.max_blocks,
                toyos_blockring::MAX_REQUEST_BLOCKS
            )));
        }
        id.zero();
        self.admin_command(
            "Identify Namespace 1",
            Command { opcode: ADMIN_IDENTIFY, nsid: 1, prp1: buf, cdw10: 0, ..Command::default() },
        )?;
        let nsze: u64 = id.read(0);
        let flbas: u8 = id.read(26);
        let format: u32 = id.read(128 + 4 * (flbas & 0x0F) as usize);
        let lbads = (format >> 16) & 0xFF;
        if !(9..=12).contains(&lbads) {
            return Err(Refusal::Geometry(format!(
                "namespace 1 has 2^{lbads}-byte sectors, and a 4 KiB block is whole sectors only \
                 for 512..=4096"
            )));
        }
        self.lba_bytes = 1 << lbads;
        self.sectors = nsze;
        Ok(())
    }

    fn make_io_queues(&mut self) -> Result<(), Refusal> {
        let asked = MAX_IO_QUEUES as u32 - 1;
        let answer = self.admin_command(
            "Set Features (Number of Queues)",
            Command {
                opcode: ADMIN_SET_FEATURES,
                cdw10: FEATURE_NUMBER_OF_QUEUES,
                cdw11: (asked << 16) | asked,
                ..Command::default()
            },
        )?;
        let sqs = (answer & 0xFFFF) + 1;
        let cqs = (answer >> 16) + 1;
        let n = sqs.min(cqs).min(MAX_IO_QUEUES as u32) as u16;
        self.io.clear();
        for q in 0..n {
            let qid = q + 1;
            let sq = OFF_IO + q as usize * 2 * PAGE;
            let cq = sq + PAGE;
            self.dma.sub(sq, PAGE).zero();
            self.dma.sub(cq, PAGE).zero();
            let size = (IO_DEPTH as u32 - 1) << 16;
            // Physically contiguous, interrupts on, on the claim's vector 0.
            self.admin_command(
                "Create I/O Completion Queue",
                Command {
                    opcode: ADMIN_CREATE_IO_CQ,
                    prp1: self.dma_addr + cq as u64,
                    cdw10: size | qid as u32,
                    cdw11: 0b11,
                    ..Command::default()
                },
            )?;
            self.admin_command(
                "Create I/O Submission Queue",
                Command {
                    opcode: ADMIN_CREATE_IO_SQ,
                    prp1: self.dma_addr + sq as u64,
                    cdw10: size | qid as u32,
                    cdw11: ((qid as u32) << 16) | 1,
                    ..Command::default()
                },
            )?;
            self.io.push(Queue::new(qid, IO_DEPTH, sq, cq));
        }
        if self.issued.len() != self.io.len() {
            self.issued = vec![0; self.io.len()];
        }
        Ok(())
    }

    pub fn queues(&self) -> usize {
        self.io.len()
    }

    /// Commands the device holds.
    pub fn busy(&self) -> usize {
        self.io.iter().map(Queue::busy).sum()
    }

    /// Whether some I/O queue can take a command.
    pub fn has_room(&self) -> bool {
        self.io.iter().any(|q| !q.free.is_empty())
    }

    /// When the oldest command still waiting was issued.
    pub fn oldest(&self) -> Option<Instant> {
        self.io.iter().flat_map(|q| q.slots.iter().flatten()).map(|s| s.since).min()
    }

    /// Write one entry into queue `queue` (`None` is the admin queue) and ring
    /// its doorbell; answers the slot's identifier.
    fn submit(&mut self, queue: Option<usize>, cmd: Command, owner: Owner, prp: impl FnOnce(u16) -> u64) -> u16 {
        let (regs, dma, stride) = (self.regs, self.dma, self.stride);
        let q = match queue {
            Some(i) => &mut self.io[i],
            None => &mut self.admin,
        };
        let cid = q.free.pop().expect("blockd: a submission with no free slot");
        q.slots[cid as usize] = Some(Slot { owner, since: Instant::now() });
        let mut cmd = cmd;
        // Written into this slot's own list page before the entry that names
        // it exists.
        if cmd.prp2 == u64::MAX {
            cmd.prp2 = prp(cid);
        }
        cmd.write(dma.sub(q.sq + q.tail as usize * SQE_BYTES, SQE_BYTES), cid);
        // The entry is the device's once the doorbell says so, and not before.
        std::sync::atomic::fence(std::sync::atomic::Ordering::Release);
        q.tail = (q.tail + 1) % q.depth;
        regs.write::<u32>(DOORBELLS + 2 * q.qid as usize * stride, q.tail as u32);
        cid
    }

    /// Read every answer queue `queue` holds into `out`.
    fn reap_queue(&mut self, queue: Option<usize>, out: &mut Vec<Done>) {
        let (regs, dma, stride) = (self.regs, self.dma, self.stride);
        let silence = &mut self.silence;
        let stray = &mut self.stray;
        let q = match queue {
            Some(i) => &mut self.io[i],
            None => &mut self.admin,
        };
        let mut moved = false;
        loop {
            let entry = dma.sub(q.cq + q.head as usize * CQE_BYTES, CQE_BYTES);
            let dw3: u32 = entry.read(12);
            if ((dw3 >> 16) & 1 != 0) != q.phase {
                break;
            }
            // The phase bit is read before the rest of the entry is believed.
            std::sync::atomic::fence(std::sync::atomic::Ordering::Acquire);
            let dw0: u32 = entry.read(0);
            let cid = (dw3 & 0xFFFF) as u16;
            let status = (dw3 >> 17) as u16;
            q.head = (q.head + 1) % q.depth;
            if q.head == 0 {
                q.phase = !q.phase;
            }
            moved = true;
            let Some(slot) = q.slots.get_mut(cid as usize).and_then(Option::take) else {
                if !*stray {
                    *stray = true;
                    println!(
                        "blockd: queue {} answered identifier {cid}, which names no command \
                         waiting; dropped, and said once",
                        q.qid
                    );
                }
                continue;
            };
            // `--silence-write`: the write the device did is read off the
            // queue and its answer not given, and its slot stays waiting, so
            // the silence this driver meets is the device's as far as anything
            // above can tell.
            let write = matches!(slot.owner, Owner::Session { write: true, .. });
            if write && let Some(n) = silence.as_mut() {
                *n -= 1;
            }
            if write && *silence == Some(0) {
                *silence = None;
                q.slots[cid as usize] = Some(slot);
                println!("blockd: WITHHELD the device's answer to a write, queue {} identifier {cid}", q.qid);
                continue;
            }
            q.free.push(cid);
            out.push(Done { owner: slot.owner, ok: status == 0, status, dw0 });
        }
        if moved {
            regs.write::<u32>(DOORBELLS + (2 * q.qid as usize + 1) * stride, q.head as u32);
        }
    }

    /// Every answer every I/O queue holds.
    pub fn reap(&mut self, out: &mut Vec<Done>) {
        for i in 0..self.io.len() {
            self.reap_queue(Some(i), out);
        }
    }

    /// Consume the claim's interrupt record. `Io` is the unit having refused
    /// this function an access, after which the claim answers nothing.
    pub fn take_interrupt(&self) -> Result<(), SyscallError> {
        match self.dev.irq() {
            Ok(_) | Err(SyscallError::WouldBlock) => Ok(()),
            Err(why) => Err(why),
        }
    }

    /// Wait for the claim's interrupt, at most `bound`.
    fn wait_interrupt(&self, bound: Duration) {
        self.poller.watch(&self.dev, READABLE, 0);
        self.poller.wait(1, bound.as_nanos() as u64, |_| {});
        if let Err(why) = self.take_interrupt() {
            panic!("blockd: the claim refused its interrupt read ({why:?}): the unit refused this controller an access");
        }
    }

    /// An admin command, waited for: bring-up and reset are the only callers,
    /// and nothing else is on the device then.
    fn admin_command(&mut self, what: &'static str, cmd: Command) -> Result<u32, Refusal> {
        let cid = self.submit(None, cmd, Owner::Driver, |_| 0);
        let start = Instant::now();
        let mut out = Vec::new();
        loop {
            self.reap_queue(None, &mut out);
            if let Some(done) = out.pop() {
                debug_assert!(out.is_empty());
                let _ = cid;
                return if done.ok { Ok(done.dw0) } else { Err(Refusal::Admin(what, done.status)) };
            }
            if start.elapsed() > self.ready_bound {
                return Err(Refusal::Silent(what, self.ready_bound));
            }
            self.wait_interrupt(self.ready_bound);
        }
    }

    /// The queue with the most room.
    fn roomiest(&self) -> Option<usize> {
        (0..self.io.len()).filter(|&i| !self.io[i].free.is_empty()).max_by_key(|&i| self.io[i].free.len())
    }

    /// Issue a read or a write of `blocks` 4 KiB blocks at device block
    /// `block`, to or from device address `at`, for `owner`.
    ///
    /// # Panics
    /// With no room: the caller asks [`Self::has_room`] first.
    pub fn submit_io(&mut self, write: bool, block: u64, blocks: u32, at: u64, owner: Owner) {
        let queue = self.roomiest().expect("blockd: an I/O submission with no queue room");
        let per = (toyos_blockring::BLOCK_BYTES as u32 / self.lba_bytes) as u64;
        let sectors = blocks as u64 * per;
        let pages = blocks as usize;
        let (dma, dma_addr) = (self.dma, self.dma_addr);
        let list_base = OFF_PRP + queue * IO_DEPTH as usize * PAGE;
        let cmd = Command {
            opcode: if write { IO_WRITE } else { IO_READ },
            nsid: 1,
            prp1: at,
            prp2: match pages {
                1 => 0,
                2 => at + PAGE as u64,
                _ => u64::MAX,
            },
            cdw10: (block * per) as u32,
            cdw11: ((block * per) >> 32) as u32,
            cdw12: (sectors - 1) as u32,
        };
        // Every page after the first, in this slot's list page (§4.1.1).
        self.submit(Some(queue), cmd, owner, |cid| {
            let list = list_base + cid as usize * PAGE;
            for i in 1..pages {
                dma.write::<u64>(list + (i - 1) * 8, at + (i * PAGE) as u64);
            }
            dma_addr + list as u64
        });
        self.issued[queue] += 1;
        self.peak = self.peak.max(self.busy());
    }

    /// Issue a Flush for `owner` (§7.1 of the NVM Command Set).
    pub fn submit_flush(&mut self, owner: Owner) {
        let queue = self.roomiest().expect("blockd: a flush with no queue room");
        self.submit(Some(queue), Command { opcode: IO_FLUSH, nsid: 1, ..Command::default() }, owner, |_| 0);
        self.issued[queue] += 1;
        self.peak = self.peak.max(self.busy());
    }

    /// Read one 4 KiB block synchronously into `out`, through this driver's
    /// own scratch page: before any session exists, for the partition table.
    pub fn read_block(&mut self, block: u64, out: &mut [u8]) -> bool {
        assert_eq!(out.len(), toyos_blockring::BLOCK_BYTES);
        let at = self.dma_addr + OFF_SCRATCH as u64;
        let ok = self.transfer(false, block, 1, at).unwrap_or_else(|why| {
            panic!("blockd: the claim refused its interrupt read ({why:?}) reading the partition table")
        });
        if ok {
            self.dma.copy_out(OFF_SCRATCH, out);
        }
        ok
    }

    /// One read or write of `blocks` at device block `block` through device
    /// address `at`, waited for, with nothing else on the device: whether the
    /// device did it, or the claim's refusal once the unit has refused this
    /// function an access — which is how a transfer aimed outside the
    /// function's domain ends.
    pub fn transfer(&mut self, write: bool, block: u64, blocks: u32, at: u64) -> Result<bool, SyscallError> {
        assert_eq!(self.busy(), 0, "blockd: a waited transfer beside other commands");
        self.submit_io(write, block, blocks, at, Owner::Driver);
        let start = Instant::now();
        let mut done = Vec::new();
        loop {
            self.reap(&mut done);
            if let Some(d) = done.pop() {
                return Ok(d.ok);
            }
            assert!(start.elapsed() < self.ready_bound * 4, "blockd: a waited transfer was not answered");
            self.poller.watch(&self.dev, READABLE, 0);
            self.poller.wait(1, self.ready_bound.as_nanos() as u64, |_| {});
            self.take_interrupt()?;
        }
    }

    /// Wait, at most `bound`, for the claim to answer with the unit's refusal
    /// — what every call on a claim answers once its function was refused an
    /// access; whether it did.
    pub fn refused_within(&self, bound: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < bound {
            if self.take_interrupt() == Err(SyscallError::Io) {
                return true;
            }
            self.poller.watch(&self.dev, READABLE, 0);
            self.poller.wait(1, bound.saturating_sub(start.elapsed()).as_nanos() as u64, |_| {});
        }
        self.take_interrupt() == Err(SyscallError::Io)
    }

    /// Take every command back by resetting the controller (§3.7.2), and
    /// make its queues again; answers whose commands they were. What the
    /// device answered before the reset is read first and goes into `out`.
    pub fn reset(&mut self, out: &mut Vec<Done>) -> Result<Vec<Owner>, Refusal> {
        self.reap(out);
        let aborted: Vec<Owner> = self
            .io
            .iter_mut()
            .flat_map(|q| q.slots.iter_mut().filter_map(Option::take))
            .map(|slot| slot.owner)
            .collect();
        self.enable()?;
        self.make_io_queues()?;
        Ok(aborted)
    }

    /// The queues each command was issued down, as a line.
    pub fn spread(&self) -> String {
        self.issued.iter().map(u64::to_string).collect::<Vec<_>>().join("/")
    }
}
