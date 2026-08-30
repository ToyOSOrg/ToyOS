//! NVMe, as a [`BlockDevice`].
//!
//! One command is outstanding at a time; `wait_completion`'s `cid` check is
//! what enforces it. [`COMMAND`] bounds one command; [`crate::block::OPERATION`]
//! bounds the composed operation a caller spends. The operation deadline,
//! established in [`NvmeBlockDevice`]'s trait methods, threads down to
//! `read_sectors`/`write_sectors`; `admin` takes none and is bounded by
//! [`COMMAND`] alone. A refusal is taken between commands, never inside one.

use core::sync::atomic::{fence, Ordering};
use toyos_untrusted::{Refused, Untrusted};
use crate::mm::Mmio;
use super::pci::PciDevice;
use super::DmaPool;
use crate::block::{self, BlockDevice, BlockError, BlockResult, DeviceId};
use crate::mm::paging::MmioPolicy;
use crate::log;
use crate::mm::{Dma, Unaligned};
use crate::scheduler::Operation;
use crate::time::{Budget, Deadline, Duration};

const REG_CAP: u64 = 0x00;
const REG_CC: u64 = 0x14;
const REG_CSTS: u64 = 0x1C;
const REG_AQA: u64 = 0x24;
const REG_ASQ: u64 = 0x28;
const REG_ACQ: u64 = 0x30;

const QUEUE_DEPTH: usize = 16;

/// How long one command may spend before this driver stops waiting for it;
/// must equal `xhci`'s `USB_TIMEOUT_NS` and [`crate::block::OPERATION`]'s own
/// budget, since both derive from the same arithmetic. A command that
/// outlasts it is reclaimed by one controller reset and one post-reset
/// chance (NVMe 2.0 §3.7.2), never by declaring the disk failed outright.
///
/// A [`Budget`], not a [`crate::time::Bound`]: NVMe defines no per-command
/// timeout — `CAP.TO` bounds the `CSTS.RDY` waits in [`NvmeController::reset`]
/// and [`init`], and nothing else.
const COMMAND: Budget = Budget::of(
    Duration::from_secs(2),
    "the command is abandoned to a controller reset, and one post-reset silence \
     marks the disk failed",
);

/// NVMe Identify Namespace (partial). `Copy`: read out of DMA memory by
/// value, never held as a reference into a window the device may write again.
#[repr(C)]
#[derive(Clone, Copy)]
struct IdentifyNamespace {
    nsze: u64,            // offset 0: namespace size in LBAs
    ncap: u64,            // offset 8: namespace capacity
    nuse: u64,            // offset 16: namespace utilization
    nsfeat: u8,           // offset 24
    nlbaf: u8,            // offset 25: number of LBA formats (0-based)
    flbas: u8,            // offset 26: formatted LBA size
    _padding: [u8; 101],  // offsets 27..128
    lba_formats: [u32; 64], // offset 128: LBA format descriptors (4 bytes each)
}

const ADMIN_CREATE_IO_SQ: u8 = 0x01;
const ADMIN_CREATE_IO_CQ: u8 = 0x05;
const ADMIN_IDENTIFY: u8 = 0x06;
const IO_WRITE: u8 = 0x01;
const IO_READ: u8 = 0x02;

/// EN plus IOSQES/IOCQES for the 64-/16-byte entry sizes above; one constant
/// so [`init`] and [`NvmeController::reset`] enable the controller identically.
const CC_ENABLED: u32 = 1 | (6 << 16) | (4 << 20);

#[repr(C)]
#[derive(Clone, Copy)]
struct SqEntry {
    cdw0: u32,
    nsid: u32,
    cdw2: u32,
    cdw3: u32,
    mptr: u64,
    prp1: u64,
    prp2: u64,
    cdw10: u32,
    cdw11: u32,
    cdw12: u32,
    cdw13: u32,
    cdw14: u32,
    cdw15: u32,
}

impl SqEntry {
    const ZERO: Self = Self {
        cdw0: 0, nsid: 0, cdw2: 0, cdw3: 0,
        mptr: 0, prp1: 0, prp2: 0,
        cdw10: 0, cdw11: 0, cdw12: 0, cdw13: 0, cdw14: 0, cdw15: 0,
    };
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CqEntry {
    dw0: u32,
    dw1: u32,
    sq_head: u16,
    sq_id: u16,
    cid: u16,
    status: u16, // bit 0 = phase, bits [15:1] = status
}

/// One submission/completion queue pair, as two [`Dma`] views: the controller
/// accesses both concurrently with this CPU, so every access goes through the
/// volatile discipline and is bounded against the view's own length.
struct NvmeQueue {
    sq: Dma<'static>,
    cq: Dma<'static>,
    sq_tail: u16,
    cq_head: u16,
    phase: bool,
    sq_doorbell: u64,
    cq_doorbell: u64,
}

impl NvmeQueue {
    fn new(sq: Dma<'static>, cq: Dma<'static>, qid: u16, stride: u32) -> Self {
        let doorbell_stride = 4u64 << stride;
        Self {
            sq, cq,
            sq_tail: 0, cq_head: 0, phase: true,
            sq_doorbell: 0x1000 + (2 * qid as u64) * doorbell_stride,
            cq_doorbell: 0x1000 + (2 * qid as u64 + 1) * doorbell_stride,
        }
    }

    /// Resets this queue's software state to freshly-created; the views and
    /// doorbell offsets stand, since the reset moves no memory.
    fn start_over(&mut self) {
        self.sq_tail = 0;
        self.cq_head = 0;
        self.phase = true;
    }

    fn submit(&mut self, bar: &Mmio, cmd: SqEntry) {
        // Bounded by `sq_tail % QUEUE_DEPTH` against the page `init` allocated;
        // the fence and doorbell below are what tell the device it happened.
        self.sq.write(self.sq_tail as usize * core::mem::size_of::<SqEntry>(), cmd);
        self.sq_tail = (self.sq_tail + 1) % QUEUE_DEPTH as u16;
        fence(Ordering::Release);
        bar.write_u32(self.sq_doorbell, self.sq_tail as u32);
    }

    /// Wait for the completion at the head of the queue; refuse it unless its
    /// `cid` is `expected`.
    ///
    /// Bounded by [`COMMAND`]: callers reach this holding `BLOCK_CACHE` and
    /// `BLOCK_DEV`, so an unbounded wait would wedge a CPU holding both.
    ///
    /// Reads the entry twice: the predicate read inside `settles` is not the
    /// read consumed below, sound because one command is outstanding at a
    /// time, so nothing rewrites the entry before the second read.
    fn wait_completion(&mut self, bar: &Mmio, expected: u16) -> Result<u16, Unanswered> {
        // Abandons this command without waiting, on the harness's request: no
        // QEMU device property makes a real completion not arrive, so this is
        // the only way to stage the state a silent controller leaves behind.
        #[cfg(feature = "boot-actuators")]
        if silent_command::take() {
            return Err(Unanswered::Silent);
        }
        let (cq, head, phase) = (self.cq, self.cq_head, self.phase);
        let at = |i: u16| i as usize * core::mem::size_of::<CqEntry>();
        let answered = crate::clock::settles(COMMAND.nanos(), || {
            // Volatile so the spin observes the phase bit flip rather than
            // reading it once (NVMe 2.0 §3.3.3.2); bounded the same way `submit`
            // is, by `cq_head % QUEUE_DEPTH`.
            let entry: CqEntry = cq.read(at(head));
            ((entry.status & 1) != 0) == phase
        });
        if !answered {
            return Err(Unanswered::Silent);
        }
        let cq: CqEntry = self.cq.read(at(self.cq_head));
        let status = cq.status >> 1;
        let cid = Untrusted::new(cq.cid);
        self.cq_head = (self.cq_head + 1) % QUEUE_DEPTH as u16;
        if self.cq_head == 0 {
            self.phase = !self.phase;
        }
        bar.write_u32(self.cq_doorbell, self.cq_head as u32);
        cid.exactly(expected).map(|_| status).map_err(Unanswered::Wrong)
    }

    fn submit_and_wait(&mut self, bar: &Mmio, cmd: SqEntry) -> Result<u16, Unanswered> {
        // `cmd`'s own cid, not a value trusted back from the caller.
        let expected = (cmd.cdw0 >> 16) as u16;
        self.submit(bar, cmd);
        self.wait_completion(bar, expected)
    }
}

/// The arm behind `nvme-command-silent`: `nvme_gate` arms this at its own
/// read, never at the boot parameter, so `init`'s own Identify wait is never
/// the one skipped. See the take site, [`NvmeQueue::wait_completion`].
#[cfg(feature = "boot-actuators")]
pub mod silent_command {
    use core::sync::atomic::{AtomicBool, Ordering};

    static ARMED: AtomicBool = AtomicBool::new(false);

    pub fn arm() {
        ARMED.store(true, Ordering::Relaxed);
    }

    pub(super) fn take() -> bool {
        ARMED.swap(false, Ordering::Relaxed)
    }
}

/// Why a submitted command produced no status this driver may use. The two
/// arms differ in what they leave behind: `Wrong` leaves the queue consistent,
/// `Silent` leaves an entry owed, reclaimed only by [`NvmeController::reset`].
enum Unanswered {
    /// The completion queue answered a different command.
    Wrong(Refused),
    /// The controller did not answer inside [`COMMAND`].
    Silent,
}

impl core::fmt::Display for Unanswered {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Wrong(refused) => {
                write!(f, "the completion queue answered a different command ({refused})")
            }
            Self::Silent => write!(f, "no completion in {}", COMMAND.duration()),
        }
    }
}

const OFF_ADMIN_SQ: usize   = 0x0000;
const OFF_ADMIN_CQ: usize   = 0x1000;
const OFF_IO_SQ: usize      = 0x2000;
const OFF_IO_CQ: usize      = 0x3000;
const OFF_IDENTIFY: usize   = 0x4000;
const OFF_PRP_LIST: usize   = 0x5000;
const OFF_DATA: usize       = 0x6000;
const MAX_DATA_PAGES: usize  = 32;
const DMA_SIZE: usize        = OFF_DATA + MAX_DATA_PAGES * 0x1000;

/// Fills the PRP list with every data page's address after the first
/// (NVMe 2.0 §4.1.2) and returns the list's own physical address for `prp2`.
/// Unaligned discipline: the list is written before the controller can read it.
fn fill_prp_list(dma: Dma<'static, Unaligned>, pages: usize, data_phys: u64) -> u64 {
    // `pages` is bounded by `MAX_DATA_PAGES`, asserted by both callers;
    // `subview` turns that bound into a check on the `pages - 1` entries.
    let list = dma.subview(OFF_PRP_LIST, (pages - 1) * core::mem::size_of::<u64>());
    for i in 1..pages {
        list.write::<u64>((i - 1) * core::mem::size_of::<u64>(), data_phys + i as u64 * 0x1000);
    }
    dma.phys() + OFF_PRP_LIST as u64
}

struct NvmeController {
    bar: Mmio,
    /// This controller's DMA window, leaked at `init` and therefore `'static`.
    dma: Dma<'static>,
    admin: NvmeQueue,
    io: NvmeQueue,
    next_cid: u16,
    sector_size: u32,
    ns_size: u64,
    /// Whether this controller has been declared failed; once set, this
    /// driver issues nothing more on it.
    failed: bool,
    /// Whether the last thing on this controller was a reset. One post-reset
    /// command is the escalation's whole allowance: silence with this set
    /// declares the controller failed; any served command clears it.
    fresh_reset: bool,
}

impl NvmeController {
    /// Clear `len` bytes of the DMA window at `off`. Every caller clears a
    /// queue, scratch page or descriptor buffer before submitting the command
    /// that hands it to the controller.
    fn zero_dma(&self, off: usize, len: usize) {
        self.dma.subview(off, len).zero();
    }

    fn alloc_cid(&mut self) -> u16 {
        let cid = self.next_cid;
        self.next_cid = self.next_cid.wrapping_add(1);
        cid
    }

    /// An admin command's silence ends the controller at once, with no reset
    /// between: admin commands run only at bring-up and inside [`Self::reset`]
    /// itself, where a reset escalation would recurse into its own failure.
    fn admin_command(&mut self, cmd: SqEntry) -> Result<u16, Unanswered> {
        let out = self.admin.submit_and_wait(&self.bar, cmd);
        if matches!(out, Err(Unanswered::Silent)) && !self.failed {
            self.failed = true;
            log!("NVMe: this controller is offline: an admin command went unanswered, and \
                 the reset escalation is not spent on a controller that cannot be asked to \
                 make queues");
        }
        out
    }

    /// An I/O command, with silence escalated: one controller reset, one
    /// post-reset chance, then the disk declared failed.
    fn io_command(&mut self, cmd: SqEntry) -> Result<u16, Unanswered> {
        let out = self.io.submit_and_wait(&self.bar, cmd);
        match &out {
            Ok(_) => self.fresh_reset = false,
            Err(Unanswered::Silent) if !self.failed => {
                if self.fresh_reset {
                    self.failed = true;
                    log!("NVMe: this controller is offline: the first command after its reset \
                         went unanswered too, and one post-reset command is the escalation's \
                         whole allowance");
                } else {
                    log!("NVMe: no completion in {} — resetting the controller: the abandoned \
                         command still owns its PRP list and is still owed a completion entry, \
                         and a reset is the one way to take both back", COMMAND.duration());
                    if self.reset() {
                        self.fresh_reset = true;
                        log!("NVMe: controller reset complete; the disk stays online and the \
                             caller is told to ask again");
                    } else {
                        self.failed = true;
                        log!("NVMe: this controller is offline: the reset escalation itself \
                             failed");
                    }
                }
            }
            Err(_) => {}
        }
        out
    }

    /// What one unanswered I/O command means to [`BlockDevice`]'s caller,
    /// decided after [`Self::io_command`]'s escalation has run: a silence the
    /// reset reclaimed is [`BlockError::BudgetExpired`], everything else is
    /// [`BlockError::Device`].
    fn unanswered(&self, why: Unanswered) -> BlockError {
        match why {
            Unanswered::Silent if !self.failed => BlockError::BudgetExpired,
            Unanswered::Silent | Unanswered::Wrong(_) => BlockError::Device,
        }
    }

    /// Controller reset: `CC.EN` 0 → 1 plus the I/O queues made afresh
    /// (NVMe 2.0 §3.7.2), reclaiming an abandoned command's PRP list and owed
    /// completion entry. The two `RDY` waits are bounded by `CAP.TO`
    /// (§3.1.4.1, 500 ms units), the controller's own published worst case.
    /// The admin queue is not re-created by command, so `AQA`/`ASQ`/`ACQ` are
    /// rewritten with the values [`init`] programmed.
    fn reset(&mut self) -> bool {
        let to = ((self.bar.read_u64(REG_CAP) >> 24) & 0xFF).max(1);
        let ready = crate::time::Bound::from_register(
            Duration::from_millis(to * 500),
            "NVMe CAP.TO, the controller's own worst case for a CSTS.RDY transition",
        );
        let cc = self.bar.read_u32(REG_CC);
        self.bar.write_u32(REG_CC, cc & !1);
        if !crate::clock::settles(ready.nanos(), || self.bar.read_u32(REG_CSTS) & 1 == 0) {
            log!("NVMe: reset failed: CSTS.RDY would not clear in {ready}");
            return false;
        }

        self.zero_dma(OFF_ADMIN_SQ, 4096);
        self.zero_dma(OFF_ADMIN_CQ, 4096);
        self.admin.start_over();
        self.io.start_over();
        let aqa = ((QUEUE_DEPTH as u32 - 1) << 16) | (QUEUE_DEPTH as u32 - 1);
        self.bar.write_u32(REG_AQA, aqa);
        self.bar.write_u64(REG_ASQ, self.dma.phys() + OFF_ADMIN_SQ as u64);
        self.bar.write_u64(REG_ACQ, self.dma.phys() + OFF_ADMIN_CQ as u64);
        self.bar.write_u32(REG_CC, CC_ENABLED);
        if !crate::clock::settles(ready.nanos(), || self.bar.read_u32(REG_CSTS) & 1 != 0) {
            log!("NVMe: reset failed: CSTS.RDY would not set in {ready}");
            return false;
        }
        // Not re-asked: writing `sector_size` mid-boot would race every
        // layout the layers above already derived from it.
        self.create_io_cq() && self.create_io_sq()
    }

    /// An admin command, with the returned status checked: discarding it would
    /// make a controller that refused indistinguishable from one that did not.
    /// No deadline argument: bringing a controller up is bounded by
    /// [`COMMAND`] alone.
    fn admin(&mut self, cmd: SqEntry, what: &str) -> bool {
        let status = match self.admin_command(cmd) {
            Ok(status) => status,
            Err(why) => {
                log!("NVMe: {what}: {why}");
                return false;
            }
        };
        if status != 0 {
            log!("NVMe: {what} failed, status={status:#x}");
            return false;
        }
        true
    }

    /// Whether this command may be issued: the controller still has its
    /// queues, and the caller's budget has something left. Read between
    /// commands and never inside one, so a refusal here costs nothing — no
    /// completion is owed and the DMA window is nobody's. An abandoned
    /// controller is [`BlockError::Device`]; a spent budget is
    /// [`BlockError::BudgetExpired`], never a fact about the controller.
    fn may_issue(&self, until: Deadline, op: &str, lba: u64, sector_count: u32) -> BlockResult {
        if self.failed {
            return Err(BlockError::Device);
        }
        if until.reached(crate::clock::now()) {
            log!("NVMe: {op} of {sector_count} sectors at {lba} not issued: {}", block::OPERATION);
            return Err(BlockError::BudgetExpired);
        }
        Ok(())
    }

    fn identify_controller(&mut self) -> bool {
        let dma = self.dma;
        let cid = self.alloc_cid();
        let mut cmd = SqEntry::ZERO;
        cmd.cdw0 = (cid as u32) << 16 | ADMIN_IDENTIFY as u32;
        cmd.prp1 = dma.phys() + OFF_IDENTIFY as u64;
        cmd.cdw10 = 1;
        self.admin(cmd, "Identify Controller")
    }

    fn create_io_cq(&mut self) -> bool {
        self.zero_dma(OFF_IO_CQ, QUEUE_DEPTH * core::mem::size_of::<CqEntry>());
        let dma = self.dma;
        let cid = self.alloc_cid();
        let mut cmd = SqEntry::ZERO;
        cmd.cdw0 = (cid as u32) << 16 | ADMIN_CREATE_IO_CQ as u32;
        cmd.prp1 = dma.phys() + OFF_IO_CQ as u64;
        cmd.cdw10 = ((QUEUE_DEPTH as u32 - 1) << 16) | 1;
        cmd.cdw11 = 1;
        self.admin(cmd, "Create I/O Completion Queue")
    }

    fn create_io_sq(&mut self) -> bool {
        self.zero_dma(OFF_IO_SQ, QUEUE_DEPTH * core::mem::size_of::<SqEntry>());
        let dma = self.dma;
        let cid = self.alloc_cid();
        let mut cmd = SqEntry::ZERO;
        cmd.cdw0 = (cid as u32) << 16 | ADMIN_CREATE_IO_SQ as u32;
        cmd.prp1 = dma.phys() + OFF_IO_SQ as u64;
        cmd.cdw10 = ((QUEUE_DEPTH as u32 - 1) << 16) | 1;
        cmd.cdw11 = (1 << 16) | 1;
        self.admin(cmd, "Create I/O Submission Queue")
    }

    fn identify_namespace(&mut self) -> bool {
        let dma = self.dma;
        self.zero_dma(OFF_IDENTIFY, 4096);
        let cid = self.alloc_cid();
        let mut cmd = SqEntry::ZERO;
        cmd.cdw0 = (cid as u32) << 16 | ADMIN_IDENTIFY as u32;
        cmd.nsid = 1;
        cmd.prp1 = dma.phys() + OFF_IDENTIFY as u64;
        cmd.cdw10 = 0;
        if !self.admin(cmd, "Identify Namespace") {
            return false;
        }

        // Copy, not a reference into memory the device may write again.
        // Unaligned discipline: `admin` returning `true` means the transfer
        // has completed, so nothing is writing these bytes (NVMe 2.0 §5.17.2.1).
        let ns: IdentifyNamespace = dma.unaligned().read(OFF_IDENTIFY);
        let fmt_idx = (ns.flbas & 0x0F) as usize;
        let lba_ds = (ns.lba_formats[fmt_idx] >> 16) & 0xFF;
        // `1 << lba_ds` overflows above 31, and above 12 `4096 / sector_size`
        // is zero, which `NvmeBlockDevice::new` divides `nsze` by — a `#DE`
        // before storage is up. 9..=12 is this driver: every path above the
        // sector layer needs 4096 to divide the sector size evenly.
        assert!(
            (9..=12).contains(&lba_ds),
            "NVMe: namespace reports 2^{lba_ds}-byte sectors (flbas={:#x}, format {fmt_idx}); \
             this driver serves 4096-byte blocks and needs 512..=4096",
            ns.flbas,
        );
        self.sector_size = 1 << lba_ds;
        self.ns_size = ns.nsze;
        log!("NVMe: NS1 size={} sectors, sector_size={}", ns.nsze, self.sector_size);
        true
    }

    /// Read `sector_count` contiguous sectors starting at `lba` into `buf`.
    /// `until` is the whole operation's deadline, not this command's.
    fn read_sectors(
        &mut self,
        lba: u64,
        sector_count: u32,
        buf: &mut [u8],
        until: Deadline,
    ) -> BlockResult {
        let total_bytes = sector_count as usize * self.sector_size as usize;
        assert!(buf.len() >= total_bytes);
        assert!(total_bytes <= MAX_DATA_PAGES * 4096);

        self.may_issue(until, "read", lba, sector_count)?;

        let dma = self.dma;
        let pages = total_bytes.div_ceil(4096);
        let data_phys = dma.phys() + OFF_DATA as u64;

        let cid = self.alloc_cid();
        let mut cmd = SqEntry::ZERO;
        cmd.cdw0 = (cid as u32) << 16 | IO_READ as u32;
        cmd.nsid = 1;
        cmd.prp1 = data_phys;
        cmd.cdw10 = lba as u32;
        cmd.cdw11 = (lba >> 32) as u32;
        cmd.cdw12 = sector_count - 1;

        if pages == 2 {
            cmd.prp2 = data_phys + 0x1000;
        } else if pages > 2 {
            cmd.prp2 = fill_prp_list(dma.unaligned(), pages, data_phys);
        }

        let status = match self.io_command(cmd) {
            Ok(status) => status,
            Err(why) => {
                log!("NVMe: read of {sector_count} sectors at {lba}: {why}");
                return Err(self.unanswered(why));
            }
        };
        if status != 0 {
            log!("NVMe: read of {sector_count} sectors at {lba} failed, status={status:#x}");
            return Err(BlockError::Device);
        }

        // Copied out rather than referenced, so nothing outlives the instant
        // the controller is known done with it; bounds asserted on entry.
        dma.copy_to(OFF_DATA, &mut buf[..total_bytes]);
        Ok(())
    }

    fn write_sectors(
        &mut self,
        lba: u64,
        sector_count: u32,
        buf: &[u8],
        until: Deadline,
    ) -> BlockResult {
        let total_bytes = sector_count as usize * self.sector_size as usize;
        assert!(buf.len() >= total_bytes);
        assert!(total_bytes <= MAX_DATA_PAGES * 4096);

        self.may_issue(until, "write", lba, sector_count)?;

        let dma = self.dma;
        let pages = total_bytes.div_ceil(4096);
        let data_phys = dma.phys() + OFF_DATA as u64;

        // Bounds asserted on entry; exclusive, since the write command naming
        // this window has not been submitted yet.
        dma.copy_from(OFF_DATA, &buf[..total_bytes]);

        let cid = self.alloc_cid();
        let mut cmd = SqEntry::ZERO;
        cmd.cdw0 = (cid as u32) << 16 | IO_WRITE as u32;
        cmd.nsid = 1;
        cmd.prp1 = data_phys;
        cmd.cdw10 = lba as u32;
        cmd.cdw11 = (lba >> 32) as u32;
        cmd.cdw12 = sector_count - 1;

        if pages == 2 {
            cmd.prp2 = data_phys + 0x1000;
        } else if pages > 2 {
            cmd.prp2 = fill_prp_list(dma.unaligned(), pages, data_phys);
        }

        let status = match self.io_command(cmd) {
            Ok(status) => status,
            Err(why) => {
                log!("NVMe: write of {sector_count} sectors at {lba}: {why}");
                return Err(self.unanswered(why));
            }
        };
        if status != 0 {
            log!("NVMe: write of {sector_count} sectors at {lba} failed, status={status:#x}");
            return Err(BlockError::Device);
        }
        Ok(())
    }
}

/// NVMe block device exposing 4KB block I/O through the BlockDevice trait.
/// `Send` is derived, not asserted: every field, including the queues'
/// [`Dma`] views, is `Send` on its own.
pub struct NvmeBlockDevice {
    ctrl: NvmeController,
    id: DeviceId,
    sectors_per_block: u32,
    block_count: u64,
}

impl NvmeBlockDevice {
    fn new(ctrl: NvmeController, id: DeviceId) -> Self {
        let sectors_per_block = 4096 / ctrl.sector_size;
        let block_count = ctrl.ns_size / sectors_per_block as u64;
        log!("NVMe: block device id={} blocks={} ({}MB)",
            id, block_count, block_count * 4096 / (1024 * 1024));
        Self { ctrl, id, sectors_per_block, block_count }
    }

    /// The namespace's own logical block size, which `BlockDevice` hides;
    /// a GPT is laid out in the device's blocks and needs it directly.
    pub fn sector_size(&self) -> u32 {
        self.ctrl.sector_size
    }
}

impl BlockDevice for NvmeBlockDevice {
    fn device_id(&self) -> DeviceId { self.id }
    fn block_count(&self) -> u64 { self.block_count }

    /// `let _op`, not `let _`: the latter drops at end of statement, ending
    /// the operation before the loop below it bounds. The deadline is read
    /// after establishment, since establishing may only narrow it.
    fn read_blocks(&mut self, lba: u64, count: u32, buf: &mut [u8]) -> BlockResult {
        assert_eq!(buf.len(), count as usize * 4096);
        let _op = block::begin_operation();
        let until = Operation::deadline();
        let mut remaining = count;
        let mut block = lba;
        let mut offset = 0usize;

        while remaining > 0 {
            let batch = remaining.min(MAX_DATA_PAGES as u32);
            let sector_lba = block * self.sectors_per_block as u64;
            let sector_count = batch * self.sectors_per_block;
            let bytes = batch as usize * 4096;

            if let Err(e) = self
                .ctrl
                .read_sectors(sector_lba, sector_count, &mut buf[offset..offset + bytes], until)
            {
                if e == BlockError::BudgetExpired {
                    block::census::budget_expired(self.id);
                }
                return Err(e);
            }

            block += batch as u64;
            offset += bytes;
            remaining -= batch;
        }
        Ok(())
    }

    fn write_blocks(&mut self, lba: u64, count: u32, buf: &[u8]) -> BlockResult {
        assert_eq!(buf.len(), count as usize * 4096);
        let _op = block::begin_operation();
        let until = Operation::deadline();
        let mut remaining = count;
        let mut block = lba;
        let mut offset = 0usize;

        while remaining > 0 {
            let batch = remaining.min(MAX_DATA_PAGES as u32);
            let sector_lba = block * self.sectors_per_block as u64;
            let sector_count = batch * self.sectors_per_block;
            let bytes = batch as usize * 4096;

            if let Err(e) = self
                .ctrl
                .write_sectors(sector_lba, sector_count, &buf[offset..offset + bytes], until)
            {
                if e == BlockError::BudgetExpired {
                    block::census::budget_expired(self.id);
                }
                return Err(e);
            }

            block += batch as u64;
            offset += bytes;
            remaining -= batch;
        }
        Ok(())
    }

    /// Writes are synchronous, so there is nothing to flush; this establishes
    /// no operation because it issues no command. A failed controller is the
    /// exception: `Ok` here would tell `page_cache::sync` writes completed
    /// that never did.
    fn flush(&mut self) -> BlockResult {
        if self.ctrl.failed {
            return Err(BlockError::Device);
        }
        Ok(())
    }
}

/// `CSTS.RDY` as this boot can see it; the actuator blinds the read, so a
/// controller that never answers is stageable on a device that always does.
fn rdy_observed(bar: &crate::mm::Mmio) -> bool {
    #[cfg(feature = "boot-actuators")]
    if crate::actuator::nvme_rdy_stuck() {
        return false;
    }
    bar.read_u32(REG_CSTS) & 1 != 0
}

/// Bring up the machine's first NVMe controller; a second is not served,
/// since `page_cache::init` takes a single `BlockDevice`.
pub fn init(devices: &[PciDevice]) -> Option<NvmeBlockDevice> {
    let pci_dev = *devices.iter().find(|d| d.matches_class(0x01, 0x08, None))?;
    log!("NVMe: found at PCI {:02x}:{:02x}.{}", pci_dev.bus, pci_dev.dev, pci_dev.func);

    // Refusal, not a panic: NVMe 2.0 §3.1 requires BAR 0 to be memory, so a
    // controller publishing otherwise has no disk this driver can drive.
    let bar_addr = match pci_dev.memory_bar(0) {
        Ok(memory) => memory.address(),
        Err(why) => {
            log!("NVMe: NOT INITIALISED at PCI {:02x}:{:02x}.{} — its registers are in BAR 0 and \
                 {}", pci_dev.bus, pci_dev.dev, pci_dev.func, why);
            return None;
        }
    };
    pci_dev.enable_bus_master();
    log!("NVMe: BAR0={:#x}", bar_addr);

    let bar = crate::mm::paging::map_mmio(bar_addr, 0x4000, MmioPolicy::Uncacheable);

    let cap = bar.read_u64(REG_CAP);
    let stride = ((cap >> 32) & 0xF) as u32;
    // The same two `CSTS.RDY` transitions `reset` bounds, on the same
    // published worst case — unbounded, a controller that never answers
    // hangs the boot with nothing on the log to say which one.
    let to = ((cap >> 24) & 0xFF).max(1);
    let ready = crate::time::Bound::from_register(
        Duration::from_millis(to * 500),
        "NVMe CAP.TO, the controller's own worst case for a CSTS.RDY transition",
    );
    let rdy = || rdy_observed(&bar);

    let cc = bar.read_u32(REG_CC);
    if cc & 1 != 0 {
        bar.write_u32(REG_CC, cc & !1);
        if !crate::clock::settles(ready.nanos(), || !rdy()) {
            log!("NVMe: NOT INITIALISED — CSTS.RDY would not clear in {ready}");
            return None;
        }
    }

    // Leaked, not held in a `static`; allocated after every refusal above so
    // a declined NVMe function costs no physical memory.
    let dma = DmaPool::alloc(DMA_SIZE).leak();
    const SQ_PAGE: usize = QUEUE_DEPTH * core::mem::size_of::<SqEntry>();
    const CQ_PAGE: usize = QUEUE_DEPTH * core::mem::size_of::<CqEntry>();
    let admin_sq = dma.subview(OFF_ADMIN_SQ, SQ_PAGE);
    let admin_cq = dma.subview(OFF_ADMIN_CQ, CQ_PAGE);
    let io_sq = dma.subview(OFF_IO_SQ, SQ_PAGE);
    let io_cq = dma.subview(OFF_IO_CQ, CQ_PAGE);

    dma.subview(OFF_ADMIN_SQ, 4096).zero();
    dma.subview(OFF_ADMIN_CQ, 4096).zero();

    let aqa = ((QUEUE_DEPTH as u32 - 1) << 16) | (QUEUE_DEPTH as u32 - 1);
    bar.write_u32(REG_AQA, aqa);
    bar.write_u64(REG_ASQ, dma.phys() + OFF_ADMIN_SQ as u64);
    bar.write_u64(REG_ACQ, dma.phys() + OFF_ADMIN_CQ as u64);

    bar.write_u32(REG_CC, CC_ENABLED);

    if !crate::clock::settles(ready.nanos(), rdy) {
        log!("NVMe: NOT INITIALISED — CSTS.RDY would not set in {ready}");
        return None;
    }
    log!("NVMe: controller enabled");

    let mut ctrl = NvmeController {
        bar,
        dma,
        admin: NvmeQueue::new(admin_sq, admin_cq, 0, stride),
        io: NvmeQueue::new(io_sq, io_cq, 1, stride),
        next_cid: 0,
        sector_size: 512,
        ns_size: 0,
        failed: false,
        fresh_reset: false,
    };

    // A controller refusing any of these has given no namespace to serve;
    // continuing would derive geometry from a zeroed DMA buffer.
    if !ctrl.identify_controller()
        || !ctrl.create_io_cq()
        || !ctrl.create_io_sq()
        || !ctrl.identify_namespace()
    {
        log!("NVMe: controller did not come up; this machine has no NVMe storage");
        return None;
    }

    Some(NvmeBlockDevice::new(ctrl, 1))
}
