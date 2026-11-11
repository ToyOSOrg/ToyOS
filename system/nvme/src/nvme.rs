//! One NVMe controller, NVMe Base Specification 2.0 throughout:
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
//! **The controller is let into its own idle power states** by Autonomous
//! Power State Transitions (Base 2.1 §5.1.25.1.6, §8.1.17.2), where it
//! supports them, with the table `nvme_apst` builds from its power state
//! descriptors under that crate's latency bound. It is set at bring-up and
//! again after every reset, which returns features to their defaults.
//!
//! **A command the controller does not answer is reclaimed by a controller
//! reset** (§3.7.2): `CC.EN` cleared, every command it held is gone, the queues
//! are made again. Whoever owned one is told it was not done. Every wait of
//! the reset is bounded, so a session's command is answered within the block
//! protocol's bound on a server's silence ([`ANSWERED_WITHIN`]).

use std::time::{Duration, Instant};

use toyos::volatile::Window;
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
/// `CC.SHN` and `CSTS.SHST` (NVMe Base 2.0 §3.1.4.5, §3.1.4.6): a normal
/// shutdown asked for, and its completion.
const CC_SHN: u32 = 0b11 << 14;
const CC_SHN_NORMAL: u32 = 0b01 << 14;
const CSTS_SHST: u32 = 0b11 << 2;
const CSTS_SHST_COMPLETE: u32 = 0b10 << 2;

const SQE_BYTES: usize = 64;
const CQE_BYTES: usize = 16;
const PAGE: usize = 4096;

const ADMIN_DEPTH: u16 = 32;
/// Entries per I/O queue; a queue holds one fewer command than it has entries.
const IO_DEPTH: u16 = 64;
/// The commands one I/O queue holds at once.
pub const COMMANDS_PER_QUEUE: u16 = IO_DEPTH - 1;
/// The most I/O queue pairs this driver asks for.
const MAX_IO_QUEUES: u16 = 4;

const ADMIN_CREATE_IO_SQ: u8 = 0x01;
const ADMIN_CREATE_IO_CQ: u8 = 0x05;
const ADMIN_IDENTIFY: u8 = 0x06;
const ADMIN_SET_FEATURES: u8 = 0x09;
const FEATURE_NUMBER_OF_QUEUES: u32 = 0x07;
const FEATURE_APST: u32 = 0x0C;
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

/// The controller's power states and the APST table built over them, read
/// once from Identify Controller.
#[derive(Clone, Debug)]
pub enum Power {
    /// APSTA's bit 0 is clear: the controller makes no autonomous transition.
    Unsupported,
    /// NPSS names more power states than Identify Controller holds: none is
    /// read, and no table is set.
    TooManyStates(u8),
    /// The states, and each one's transition.
    Autonomous { states: Vec<nvme_apst::PowerState>, plan: Vec<Option<nvme_apst::Transition>> },
}

/// How long a command may go unanswered before the controller is reset to
/// take it back. Policy: NVMe defines no per-command bound, and a flush of a
/// large cache is the slowest thing a healthy drive does.
pub const COMMAND_SILENCE: Duration = Duration::from_secs(10);

/// How long an admin command is waited for, at bring-up and in a reset: the
/// same policy as any other command's.
const ADMIN_BOUND: Duration = COMMAND_SILENCE;

/// `CAP.TO`'s unit, and its largest value: the controller's own bound on a
/// `CSTS.RDY` transition, in 500 ms units, at most 255 (NVMe Base 2.0
/// §3.1.4.1).
const READY_UNIT: Duration = Duration::from_millis(500);
const READY_BOUND_MOST: Duration = READY_UNIT.checked_mul(255).unwrap();

/// One admin command of a reset, in the order it is sent: the queue count, a
/// completion and a submission queue for each I/O queue the controller grants,
/// and the APST table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    QueueCount,
    CompletionQueue(u16),
    SubmissionQueue(u16),
    PowerTable,
}

impl Step {
    fn named(self) -> &'static str {
        match self {
            Self::QueueCount => "Set Features (Number of Queues)",
            Self::CompletionQueue(_) => "Create I/O Completion Queue",
            Self::SubmissionQueue(_) => "Create I/O Submission Queue",
            Self::PowerTable => "Set Features (Autonomous Power State Transition)",
        }
    }
}

/// Every step [`Controller::configure`] takes, at bring-up and in a reset,
/// each at most one admin command: the reset waits for no other.
const STEPS: [Step; 2 + 2 * MAX_IO_QUEUES as usize] = {
    let mut steps = [Step::QueueCount; 2 + 2 * MAX_IO_QUEUES as usize];
    let mut q = 0;
    while q < MAX_IO_QUEUES {
        steps[1 + 2 * q as usize] = Step::CompletionQueue(q);
        steps[2 + 2 * q as usize] = Step::SubmissionQueue(q);
        q += 1;
    }
    steps[steps.len() - 1] = Step::PowerTable;
    steps
};

/// The most admin commands a reset waits for.
const RESET_ADMIN_COMMANDS: u32 = STEPS.len() as u32;

/// The longest a command goes unanswered: [`COMMAND_SILENCE`], then the reset,
/// whose two `CSTS.RDY` transitions are each bounded by `CAP.TO` and whose
/// admin commands each by [`ADMIN_BOUND`].
const ANSWERED_WITHIN: Duration = COMMAND_SILENCE
    .checked_add(READY_BOUND_MOST.checked_mul(2).unwrap())
    .unwrap()
    .checked_add(ADMIN_BOUND.checked_mul(RESET_ADMIN_COMMANDS).unwrap())
    .unwrap();
const _: () = assert!(
    ANSWERED_WITHIN.as_nanos() <= toyos_blockring::SILENCE_BOUND_NS as u128,
    "a command the controller leaves silent would be answered past the block protocol's bound"
);

/// The submission and completion queue pages of I/O queue `q`.
fn queue_pages(q: u16) -> (usize, usize) {
    let sq = OFF_IO + usize::from(q) * 2 * PAGE;
    (sq, sq + PAGE)
}

/// What `step` answers, asked until it answers or `bound` has passed by
/// `elapsed`. `step` is given how long it may wait before it looks: nothing
/// the first time, then never more than is left of the bound, so a wake just
/// before the bound buys no second one.
fn within<T>(bound: Duration, elapsed: impl Fn() -> Duration, mut step: impl FnMut(Duration) -> Option<T>) -> Option<T> {
    let mut left = Duration::ZERO;
    loop {
        if let Some(answer) = step(left) {
            return Some(answer);
        }
        left = bound.saturating_sub(elapsed());
        if left.is_zero() {
            return None;
        }
    }
}

/// Whose a command is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    /// This driver's own, waited for in place.
    Driver,
    Session(toyos_blockserve::Owner),
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

/// Refuse a controller whose smallest memory page is not 4 KiB: `CC.MPS` is
/// written 0 and `MDTS` is read in 4 KiB units, both of which `CAP.MPSMIN`
/// (bits 51:48, a page of `2^(12 + MPSMIN)` bytes; Base 2.0 §3.1.4.1) must
/// allow.
fn page_size(cap: u64) -> Result<(), String> {
    match (cap >> 48) & 0xF {
        0 => Ok(()),
        min => Err(format!("the controller's smallest memory page is {} KiB, and this driver's is 4 KiB", 4 << min)),
    }
}

/// Namespace 1's sector size as a shift, from Identify Namespace's `NLBAF`
/// (byte 25, a zero-based count), `FLBAS` (byte 26) and the LBA format at
/// each byte offset `format_at` reads (NVM Command Set 1.0 §4.1.5.1). The
/// format index is `FLBAS` bits 3:0 with bits 6:5 above them, which a
/// namespace of 16 formats or fewer clears. A format carrying metadata is
/// refused: its sectors move more than `2^LBADS` bytes, or need a metadata
/// pointer this driver never sets.
fn sector_shift(nlbaf: u8, flbas: u8, format_at: impl Fn(usize) -> u32) -> Result<u32, String> {
    let formats = nlbaf as usize + 1;
    let index = (flbas & 0x0F) as usize | (((flbas >> 5) & 0b11) as usize) << 4;
    if index >= formats {
        return Err(format!("namespace 1 is in LBA format {index}, and it reports {formats}"));
    }
    let format = format_at(128 + 4 * index);
    let metadata = format & 0xFFFF;
    if metadata != 0 {
        return Err(format!("namespace 1's format carries {metadata} bytes of metadata per sector"));
    }
    let lbads = (format >> 16) & 0xFF;
    if !(9..=12).contains(&lbads) {
        return Err(format!(
            "namespace 1 has 2^{lbads}-byte sectors, and a 4 KiB block is whole sectors only for 512..=4096"
        ));
    }
    Ok(lbads)
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
    /// `CAP.TO`: the controller's own worst case for a `CSTS.RDY` transition,
    /// at most [`READY_BOUND_MOST`].
    ready_bound: Duration,
    admin: Queue,
    io: Vec<Queue>,
    pub lba_bytes: u32,
    pub sectors: u64,
    pub vwc: bool,
    /// The most 4 KiB blocks one command may move (`MDTS`).
    pub max_blocks: u32,
    pub power: Power,
    poller: Poller,
    /// A device answer naming no waiting slot, said once.
    stray: bool,
    /// The most commands the device has held at once.
    pub peak: usize,
    /// Commands issued down each I/O queue.
    pub issued: Vec<u64>,
}

impl Controller {
    /// Bring the controller up (§3.5.1): disable, program the admin queue,
    /// enable, identify, make the I/O queue pairs and set the APST table.
    /// `info` is the claim's one description. With the controller, the
    /// status it refused the table with, if it did.
    pub fn open(dev: PciDev, info: &toyos_abi::pci::PciFunctionInfo) -> Result<(Self, Option<u16>), Refusal> {
        let class = dev
            .config_read(0x08, toyos_abi::syscall::RegWidth::U32)
            .map_err(|e| Refusal::Kernel("the class code", e))?;
        if class >> 8 != 0x01_08_02 {
            return Err(Refusal::NotNvme(class >> 8));
        }
        let bar_bytes = info.bar_bytes[0];
        if bar_bytes <= DOORBELLS as u64 {
            return Err(Refusal::NoRegisters);
        }
        let bar = dev.map_bar(0, bar_bytes).map_err(|e| Refusal::Kernel("the register window", e))?;
        // SAFETY: the mapping is `bar_bytes` long and lives as long as `bar`,
        // which this struct holds.
        let regs = unsafe { Window::new(bar.as_ptr(), bar_bytes as usize) };
        let cap: u64 = regs.read(REG_CAP);
        page_size(cap).map_err(Refusal::Geometry)?;
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
            ready_bound: READY_UNIT.saturating_mul(to as u32),
            admin: Queue::new(0, ADMIN_DEPTH, OFF_ADMIN_SQ, OFF_ADMIN_CQ),
            io: Vec::new(),
            lba_bytes: 0,
            sectors: 0,
            vwc: false,
            max_blocks: 0,
            power: Power::Unsupported,
            poller: Poller::new(1),
            stray: false,
            peak: 0,
            issued: Vec::new(),
        };
        ctrl.enable()?;
        ctrl.identify()?;
        let refused = ctrl.configure()?;
        Ok((ctrl, refused))
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
        let settled = within(self.ready_bound, || start.elapsed(), |_| {
            let csts = self.csts();
            if (csts & CSTS_RDY != 0) == ready {
                return Some(Ok(()));
            }
            if csts & CSTS_CFS != 0 {
                return Some(Err(Refusal::Fatal(what)));
            }
            std::thread::yield_now();
            None
        });
        settled.unwrap_or(Err(Refusal::Silent(what, self.ready_bound)))
    }

    /// The machine stops: a normal shutdown (NVMe Base 2.0 §3.6.2), which has
    /// the controller make what it holds durable and finish, answered by
    /// `CSTS.SHST` within `within`. Polled: the register raises no interrupt.
    pub fn shut_down(&self, within: Duration) -> bool {
        let cc: u32 = self.regs.read(REG_CC);
        self.regs.write::<u32>(REG_CC, (cc & !CC_SHN) | CC_SHN_NORMAL);
        let start = Instant::now();
        while self.csts() & CSTS_SHST != CSTS_SHST_COMPLETE {
            if start.elapsed() > within {
                return false;
            }
            std::thread::yield_now();
        }
        true
    }

    /// Disable, program the admin queue, and enable.
    fn enable(&mut self) -> Result<(), Refusal> {
        let cc: u32 = self.regs.read(REG_CC);
        if cc & 1 != 0 {
            self.regs.write::<u32>(REG_CC, cc & !1);
        }
        self.settle(false, "CSTS.RDY clearing")?;
        // Disabled, so it runs no queue a previous holder or firmware left.
        self.dev.dma_start().map_err(|e| Refusal::Kernel("the bus-master start", e))?;
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
        let mut page = [0u8; nvme_apst::IDENTIFY_BYTES];
        id.copy_out(0, &mut page);
        self.power = match nvme_apst::states(&page) {
            Err(nvme_apst::TooManyStates(npss)) => Power::TooManyStates(npss),
            Ok((false, _)) => Power::Unsupported,
            Ok((true, states)) => Power::Autonomous { plan: nvme_apst::plan(&states), states },
        };
        let mdts: u8 = id.read(77);
        let vwc: u8 = id.read(525);
        self.vwc = vwc & 1 != 0;
        // A power of two in units of the minimum page, which `page_size`
        // holds at 4 KiB; zero is no limit.
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
        let lbads = sector_shift(id.read(25), id.read(26), |at| id.read(at)).map_err(Refusal::Geometry)?;
        self.lba_bytes = 1 << lbads;
        self.sectors = nsze;
        Ok(())
    }

    /// Make the I/O queues and set the APST table Identify built, at bring-up
    /// and after a reset: the admin command of each of [`STEPS`] that applies
    /// and no other. The answer is the status the controller refused the
    /// table with, if it did, after which it serves from its operational
    /// state.
    fn configure(&mut self) -> Result<Option<u16>, Refusal> {
        let asked = u32::from(MAX_IO_QUEUES) - 1;
        let size = (u32::from(IO_DEPTH) - 1) << 16;
        let mut granted = 0;
        let mut refused = None;
        self.io.clear();
        for step in STEPS {
            let cmd = match step {
                Step::QueueCount => Command {
                    opcode: ADMIN_SET_FEATURES,
                    cdw10: FEATURE_NUMBER_OF_QUEUES,
                    cdw11: (asked << 16) | asked,
                    ..Command::default()
                },
                // Physically contiguous, interrupts on, on the claim's vector 0.
                Step::CompletionQueue(q) if q < granted => {
                    let (_, cq) = queue_pages(q);
                    self.dma.sub(cq, PAGE).zero();
                    Command {
                        opcode: ADMIN_CREATE_IO_CQ,
                        prp1: self.dma_addr + cq as u64,
                        cdw10: size | u32::from(q + 1),
                        cdw11: 0b11,
                        ..Command::default()
                    }
                }
                Step::SubmissionQueue(q) if q < granted => {
                    let (sq, _) = queue_pages(q);
                    self.dma.sub(sq, PAGE).zero();
                    Command {
                        opcode: ADMIN_CREATE_IO_SQ,
                        prp1: self.dma_addr + sq as u64,
                        cdw10: size | u32::from(q + 1),
                        cdw11: (u32::from(q + 1) << 16) | 1,
                        ..Command::default()
                    }
                }
                Step::CompletionQueue(_) | Step::SubmissionQueue(_) => continue,
                // Through the identify page, which nothing reads after
                // bring-up; nothing is sent where Identify built no table. A
                // table with no transition is sent with APST disabled (APSTE,
                // CDW11 bit 0).
                Step::PowerTable => {
                    let Power::Autonomous { plan, .. } = &self.power else { continue };
                    let enable = plan.iter().any(Option::is_some);
                    self.dma.copy_in(OFF_IDENTIFY, &nvme_apst::encode(plan));
                    Command {
                        opcode: ADMIN_SET_FEATURES,
                        prp1: self.dma_addr + OFF_IDENTIFY as u64,
                        cdw10: FEATURE_APST,
                        cdw11: u32::from(enable),
                        ..Command::default()
                    }
                }
            };
            match (step, self.admin_command(step.named(), cmd)) {
                (Step::QueueCount, Ok(answer)) => {
                    let (sqs, cqs) = ((answer & 0xFFFF) + 1, (answer >> 16) + 1);
                    granted = sqs.min(cqs).min(u32::from(MAX_IO_QUEUES)) as u16;
                }
                (Step::SubmissionQueue(q), Ok(_)) => {
                    let (sq, cq) = queue_pages(q);
                    self.io.push(Queue::new(q + 1, IO_DEPTH, sq, cq));
                }
                (Step::PowerTable, Err(Refusal::Admin(_, status))) => refused = Some(status),
                (_, Ok(_)) => {}
                (_, Err(why)) => return Err(why),
            }
        }
        if self.issued.len() != self.io.len() {
            self.issued = vec![0; self.io.len()];
        }
        Ok(refused)
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
        let cid = q.free.pop().expect("nvme: a submission with no free slot");
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
                        "nvme: queue {} answered identifier {cid}, which names no command \
                         waiting; dropped, and said once",
                        q.qid
                    );
                }
                continue;
            };
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
            panic!("nvme: the claim refused its interrupt read ({why:?}): the unit refused this controller an access");
        }
    }

    /// An admin command, waited for: bring-up and reset are the only callers,
    /// and nothing else is on the device then.
    fn admin_command(&mut self, what: &'static str, cmd: Command) -> Result<u32, Refusal> {
        self.submit(None, cmd, Owner::Driver, |_| 0);
        let start = Instant::now();
        let mut out = Vec::new();
        let done = within(ADMIN_BOUND, || start.elapsed(), |left| {
            if !left.is_zero() {
                self.wait_interrupt(left);
            }
            self.reap_queue(None, &mut out);
            let done = out.pop();
            debug_assert!(out.is_empty());
            done
        });
        match done {
            Some(done) if done.ok => Ok(done.dw0),
            Some(done) => Err(Refusal::Admin(what, done.status)),
            None => Err(Refusal::Silent(what, ADMIN_BOUND)),
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
        let per = (toyos_blockring::BLOCK_BYTES as u32 / self.lba_bytes) as u64;
        self.submit_sectors(write, block * per, blocks as u64 * per, at, owner);
    }

    /// [`Self::submit_io`] in the namespace's own sectors: `sectors` of them
    /// from `lba`, which a table at the device's end needs when its sector
    /// count is not whole blocks.
    fn submit_sectors(&mut self, write: bool, lba: u64, sectors: u64, at: u64, owner: Owner) {
        let queue = self.roomiest().expect("nvme: an I/O submission with no queue room");
        let pages = (sectors * self.lba_bytes as u64).div_ceil(PAGE as u64) as usize;
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
            cdw10: lba as u32,
            cdw11: (lba >> 32) as u32,
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
        let queue = self.roomiest().expect("nvme: a flush with no queue room");
        self.submit(Some(queue), Command { opcode: IO_FLUSH, nsid: 1, ..Command::default() }, owner, |_| 0);
        self.issued[queue] += 1;
        self.peak = self.peak.max(self.busy());
    }

    /// Read `out.len()` bytes of whole sectors from `lba`, at most a page,
    /// synchronously through this driver's own scratch page: while no
    /// session's command is on the device, for the partition table.
    pub fn read_sectors(&mut self, lba: u64, out: &mut [u8]) -> bool {
        assert!(out.len() <= PAGE && out.len().is_multiple_of(self.lba_bytes as usize) && !out.is_empty());
        assert_eq!(self.busy(), 0, "nvme: a waited read beside other commands");
        let sectors = (out.len() / self.lba_bytes as usize) as u64;
        self.submit_sectors(false, lba, sectors, self.dma_addr + OFF_SCRATCH as u64, Owner::Driver);
        let start = Instant::now();
        let mut done = Vec::new();
        let ok = loop {
            self.reap(&mut done);
            if let Some(d) = done.pop() {
                break d.ok;
            }
            assert!(start.elapsed() < self.ready_bound * 4, "nvme: a waited read was not answered");
            self.wait_interrupt(self.ready_bound);
        };
        if ok {
            self.dma.copy_out(OFF_SCRATCH, out);
        }
        ok
    }

    /// Take every command back by resetting the controller (§3.7.2), and
    /// make its queues and set its APST table again; answers whose commands
    /// they were, and the status the controller refused the table with, if
    /// it did. What the device answered before the reset is read first and
    /// goes into `out`.
    pub fn reset(&mut self, out: &mut Vec<Done>) -> Result<(Vec<Owner>, Option<u16>), Refusal> {
        self.reap(out);
        let aborted: Vec<Owner> = self
            .io
            .iter_mut()
            .flat_map(|q| q.slots.iter_mut().filter_map(Option::take))
            .map(|slot| slot.owner)
            .collect();
        self.enable()?;
        let refused = self.configure()?;
        Ok((aborted, refused))
    }

    /// The queues each command was issued down, as a line.
    pub fn spread(&self) -> String {
        self.issued.iter().map(u64::to_string).collect::<Vec<_>>().join("/")
    }
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;
    use std::time::Duration;

    use super::{page_size, sector_shift, within, Step, ANSWERED_WITHIN, STEPS};

    /// A command left silent is answered `Device` after 10 s and a reset of two
    /// `CSTS.RDY` transitions at the largest `CAP.TO`, 127.5 s each, and ten
    /// admin commands at 10 s each: 365 s, the block protocol's bound.
    #[test]
    fn a_command_left_silent_is_answered_within_365_s() {
        assert_eq!(ANSWERED_WITHIN, Duration::from_secs(365));
        assert_eq!(ANSWERED_WITHIN.as_nanos(), u128::from(toyos_blockring::SILENCE_BOUND_NS));
    }

    /// The queue count, then each of four queues' completion and submission
    /// queue, then the APST table.
    #[test]
    fn a_reset_steps_through_the_queues_then_the_power_table() {
        let mut want = vec![Step::QueueCount];
        for q in 0..4 {
            want.extend([Step::CompletionQueue(q), Step::SubmissionQueue(q)]);
        }
        want.push(Step::PowerTable);
        assert_eq!(STEPS[..], want[..]);
    }

    /// A wake for something else 1 ms before the bound, then nothing: the wait
    /// after it is for the 1 ms left, so the whole takes the bound.
    #[test]
    fn a_wake_before_the_bound_buys_no_second_bound() {
        let bound = Duration::from_secs(10);
        let clock = Cell::new(Duration::ZERO);
        let mut waits = Vec::new();
        let answer: Option<()> = within(bound, || clock.get(), |left| {
            waits.push(left);
            let woke = if waits.len() == 2 { left.saturating_sub(Duration::from_millis(1)) } else { left };
            clock.set(clock.get() + woke);
            None
        });
        assert_eq!(answer, None);
        assert_eq!(clock.get(), bound);
        assert_eq!(waits, [Duration::ZERO, bound, Duration::from_millis(1)]);
    }

    /// An answer at the first look ends the wait without one, and an answer
    /// after a wait is the answer.
    #[test]
    fn an_answer_ends_the_wait() {
        assert_eq!(within(Duration::from_secs(10), || Duration::ZERO, |_| Some(7)), Some(7));
        let clock = Cell::new(Duration::ZERO);
        let mut looks = 0;
        let answer = within(Duration::from_secs(10), || clock.get(), |left| {
            looks += 1;
            clock.set(clock.get() + left.min(Duration::from_secs(3)));
            (looks == 3).then_some("done")
        });
        assert_eq!((answer, looks, clock.get()), (Some("done"), 3, Duration::from_secs(6)));
    }

    /// An Identify Namespace page: `formats` LBA formats, each `(MS, LBADS)`.
    fn namespace(formats: &[(u16, u8)]) -> impl Fn(usize) -> u32 {
        let formats = formats.to_vec();
        move |at| {
            let (ms, lbads) = formats[(at - 128) / 4];
            ms as u32 | (lbads as u32) << 16
        }
    }

    #[test]
    fn a_namespace_past_sixteen_formats_takes_the_index_high_bits() {
        let mut formats = vec![(0, 9); 64];
        formats[0x23] = (0, 12);
        assert_eq!(sector_shift(63, 0b0100_0011, namespace(&formats)), Ok(12));
        assert_eq!(sector_shift(63, 0b0000_0011, namespace(&formats)), Ok(9));
        // Bit 4 between the two halves is not the index's.
        assert_eq!(sector_shift(63, 0b0101_0011, namespace(&formats)), Ok(12));
    }

    #[test]
    fn a_format_index_past_the_count_is_refused() {
        let formats = vec![(0, 9); 16];
        assert!(sector_shift(1, 2, namespace(&formats)).unwrap_err().contains("LBA format 2"));
        assert!(sector_shift(15, 0b0010_0011, namespace(&formats)).unwrap_err().contains("LBA format 19"));
    }

    #[test]
    fn a_format_with_metadata_is_refused_extended_or_separate() {
        let formats = [(8, 9), (64, 12)];
        // FLBAS bit 4 set: metadata interleaved with the data.
        assert!(sector_shift(1, 0b1_0000, namespace(&formats)).unwrap_err().contains("8 bytes of metadata"));
        // Clear: a separate buffer.
        assert!(sector_shift(1, 1, namespace(&formats)).unwrap_err().contains("64 bytes of metadata"));
    }

    #[test]
    fn a_sector_outside_512_to_4096_bytes_is_refused() {
        assert!(sector_shift(0, 0, namespace(&[(0, 13)])).is_err());
        assert_eq!(sector_shift(0, 0, namespace(&[(0, 9)])), Ok(9));
    }

    #[test]
    fn a_controller_whose_smallest_page_is_not_4_kib_is_refused() {
        assert_eq!(page_size(0x0000_0020_FF01_003F), Ok(()));
        let cap = 0x0001_0020_FF01_003F | (0xF << 52);
        assert!(page_size(cap).unwrap_err().contains("8 KiB"));
    }
}
