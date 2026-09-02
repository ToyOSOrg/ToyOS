//! Queued invalidation: submits a descriptor and an Invalidation Wait
//! descriptor, then polls the wait's status write to completion.
//! Acknowledgement timeout is a panic — the kernel cannot say what a device
//! can reach past it. Only the queued-invalidation path is implemented: every
//! unit reachable here has it, so a `CCMD_REG`/`IOTLB_REG` fallback would be
//! dead code; a unit without `ECAP.QI` is left unprogrammed.

use crate::mm::Mmio;
use crate::time::{Duration, Tripwire};

use super::table::{Table, Tables};
use super::{FSTS_REG, IQA_REG, IQT_REG};

/// 4 KiB of descriptors: 256 entries of 16 bytes (`IQA.QS = 0`).
const QUEUE_ENTRIES: usize = 256;

const CONTEXT_CACHE: u64 = 0x1;
const IOTLB: u64 = 0x2;
/// Type 4h, Section 6.5.2.7. Its granularity bit is inverted against the two
/// above: clear is global, set is index-selective.
const INTERRUPT_ENTRY_CACHE: u64 = 0x4;
const WAIT: u64 = 0x5;

/// Granularity field, bits 5:4: `01` is global — every domain, every entry.
const GLOBAL: u64 = 1 << 4;
/// Set on every IOTLB invalidation: waits for in-flight transactions against
/// the old translation to finish, not just for the entry to be gone.
const DRAIN_WRITES: u64 = 1 << 6;
const DRAIN_READS: u64 = 1 << 7;

const WAIT_STATUS_WRITE: u64 = 1 << 5;
const WAIT_DONE: u32 = 1;

/// `FSTS` bits meaning the queue itself failed; any of them stalls the head,
/// so a wait that never completes says which one rather than only that it waited.
const FSTS_QUEUE_ERRORS: u32 = (1 << 4) | (1 << 5) | (1 << 6);

const ACK_TIMEOUT: Tripwire = Tripwire::absurd(
    Duration::from_secs(1),
    "Linux waits one second for the same acknowledgement, and what a device can reach is unknown past it",
);

pub struct Queue {
    descriptors: Table,
    /// Own page: the ring uses its full 4 KiB, so an inline status word would collide with a descriptor slot.
    status: Table,
    tail: usize,
}

impl Queue {
    /// The caller enables `GCMD.QIE` only after this returns, since a queue the unit has not been pointed at is a queue it never reads.
    pub fn new(tables: &mut Tables, regs: Mmio) -> Self {
        let queue = Self { descriptors: tables.alloc(), status: tables.alloc(), tail: 0 };
        // Descriptor width 0, queue size 0: the register is the base address alone.
        regs.write_u64(IQA_REG, queue.descriptors.phys());
        regs.write_u64(IQT_REG, 0);
        queue
    }

    /// Every cached translation and context entry, gone, acknowledged by the
    /// unit before this returns.
    pub fn invalidate_all(&mut self, regs: Mmio) {
        self.submit(
            regs,
            &[
                (CONTEXT_CACHE | GLOBAL, 0),
                (IOTLB | GLOBAL | DRAIN_WRITES | DRAIN_READS, 0),
            ],
        );
    }

    /// Every cached interrupt remapping entry, gone. Owed after `SIRTP` on a
    /// unit whose `CAP.ESIRTPS` is clear (Section 6.7), and submitted only to a
    /// unit that remaps: a type this unit does not implement stalls its queue.
    pub fn invalidate_interrupts(&mut self, regs: Mmio) {
        self.submit(regs, &[(INTERRUPT_ENTRY_CACHE, 0)]);
    }

    fn submit(&mut self, regs: Mmio, descriptors: &[(u64, u64)]) {
        assert!(
            descriptors.len() + 1 < QUEUE_ENTRIES,
            "iommu: {} descriptors do not fit a {QUEUE_ENTRIES}-entry queue",
            descriptors.len()
        );
        self.status.write_u32(0, 0);
        for &(lo, hi) in descriptors {
            self.descriptors.write_pair(self.tail, lo, hi);
            self.tail = (self.tail + 1) % QUEUE_ENTRIES;
        }
        self.descriptors.write_pair(
            self.tail,
            WAIT | WAIT_STATUS_WRITE | ((WAIT_DONE as u64) << 32),
            self.status.phys(),
        );
        self.tail = (self.tail + 1) % QUEUE_ENTRIES;

        regs.write_u64(IQT_REG, (self.tail * 16) as u64);

        let deadline = crate::clock::nanos_since_boot() + ACK_TIMEOUT.nanos();
        while self.status.read_device_u32(0) != WAIT_DONE {
            let faults = regs.read_u32(FSTS_REG);
            assert!(
                faults & FSTS_QUEUE_ERRORS == 0,
                "iommu: the unit rejected an invalidation descriptor, FSTS={faults:#010x}"
            );
            assert!(
                crate::clock::nanos_since_boot() < deadline,
                "iommu: the unit did not acknowledge an invalidation within {} ns, \
                 FSTS={faults:#010x}",
                ACK_TIMEOUT.nanos()
            );
            core::hint::spin_loop();
        }
    }
}
