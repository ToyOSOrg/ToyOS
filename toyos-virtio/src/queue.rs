//! One split virtqueue (§2.7): its three parts in the grant, the chains in
//! flight, and what a used-ring element has to satisfy.
//!
//! **A chain is the caller's own run of descriptors**, `head` and the ones
//! after it, and the caller picks the head: a driver that gives buffer `i` the
//! head `i` gets back, in a completion's head, the buffer it filled.
//!
//! **Nothing the device can write is read back but the used ring.** The
//! descriptor table and the available ring are the driver's (§2.7.5.1 has the
//! device write neither) and the device reaches both all the same, so what is
//! in flight, how long each chain is and where the available index stands are
//! kept here, in memory the device does not reach. A descriptor table the
//! device has rewritten into a loop is therefore a loop nothing follows.
//!
//! **The order of a publication is §2.7.13's**: the descriptors and the ring
//! entry, a barrier, the index, a barrier, and only then the notification —
//! which [`Published`] is owed for and only a live transport sends.

use alloc::vec;
use alloc::vec::Vec;

use toyos_untrusted::{Refused, Untrusted};

use crate::DmaBuffers;

/// §2.7: "The maximum Queue Size value is 32768."
pub const MAX_QUEUE_SIZE: u16 = 32768;

/// §2.7.5: one `virtq_desc`.
pub const DESC_BYTES: usize = 16;
const DESC_LEN: usize = 8;
const DESC_FLAGS: usize = 12;
const DESC_NEXT: usize = 14;
const VIRTQ_DESC_F_NEXT: u16 = 1;
const VIRTQ_DESC_F_WRITE: u16 = 2;

/// §2.7.6 and §2.7.8: both rings open with `flags` and `idx`, sixteen bits
/// each — `flags` zeroed with the ring and never written again — and the
/// entries follow.
const RING_IDX: usize = 2;
pub const RING_ENTRIES: usize = 4;
pub const AVAIL_ENTRY_BYTES: usize = 2;
/// §2.7.8: one `virtq_used_elem`, `id` then `len`.
pub const USED_ELEM_BYTES: usize = 8;
const USED_ELEM_LEN: usize = 4;

/// §2.7's table of sizes. The rings' include the event word that follows the
/// entries, which only `VIRTIO_F_EVENT_IDX` gives a meaning.
pub const fn desc_bytes(size: u16) -> usize {
    DESC_BYTES * size as usize
}

pub const fn avail_bytes(size: u16) -> usize {
    6 + AVAIL_ENTRY_BYTES * size as usize
}

pub const fn used_bytes(size: u16) -> usize {
    6 + USED_ELEM_BYTES * size as usize
}

/// Where a queue's three parts start in the grant.
#[derive(Clone, Copy)]
pub struct Parts {
    pub desc: usize,
    pub avail: usize,
    pub used: usize,
}

impl Parts {
    /// The three parts one after another from `at`, each on the alignment
    /// §2.7's table gives it where `at` is on the descriptor table's.
    pub const fn contiguous(at: usize, size: u16) -> Self {
        let avail = at + desc_bytes(size);
        let used = (avail + avail_bytes(size) + 3) & !3;
        Self { desc: at, avail, used }
    }

    /// One past the used ring's last byte.
    pub const fn end(&self, size: u16) -> usize {
        self.used + used_bytes(size)
    }
}

/// One element of a chain: `len` bytes at the device's `addr`, for the device
/// to read or to write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Buffer {
    pub addr: u64,
    pub len: u32,
    pub writable: bool,
}

impl Buffer {
    pub const fn readable(addr: u64, len: u32) -> Self {
        Self { addr, len, writable: false }
    }

    pub const fn writable(addr: u64, len: u32) -> Self {
        Self { addr, len, writable: true }
    }
}

/// A chain the device has finished with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Used {
    pub head: u16,
    /// Bytes the device says it wrote, never more than the chain's writable
    /// elements hold. §2.7.8.3: nothing past them is to be believed.
    pub written: u32,
}

/// Why the used ring was not believed. The device wrote every word of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsedRefusal {
    /// An element's `id` is past the descriptor table. The element is taken
    /// and names no chain.
    Head(Refused),
    /// An element's `id` is a head no chain is in flight at: one the device
    /// already gave back, or one never published. The element is taken.
    NoChain { head: u16 },
    /// An element's `len` is more than its chain's device-writable elements
    /// hold (§2.7.8). The element is taken and its chain stays in flight: its
    /// buffers are the device's still, and are not handed up.
    Written { head: u16, len: Refused },
    /// The used index stands further on than the chains made available: the
    /// device says it used buffers it was never given, so no element from the
    /// last one taken on is one this driver can tell from a stale one.
    /// **Nothing is taken, and the ring answers this until the device takes
    /// the index back**: the queue is not one to go on reading.
    Jumped { used: u16, taken: u16, available: u16 },
}

impl core::fmt::Display for UsedRefusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Head(why) => write!(f, "a used element's head {why}"),
            Self::NoChain { head } => {
                write!(f, "a used element names head {head}, where no chain is in flight")
            }
            Self::Written { head, len } => write!(
                f,
                "the used element for head {head} claims more bytes than its chain may be \
                 written: {len}"
            ),
            Self::Jumped { used, taken, available } => write!(
                f,
                "the used index reads {used}, past the available index {available}, with the \
                 elements before {taken} taken"
            ),
        }
    }
}

/// A chain made visible to the device, and the notification owed for it:
/// [`crate::pci::Live::notify`] takes it, and nothing else does.
#[must_use = "a published chain the device is never told of is never taken"]
pub struct Published {
    queue: u16,
}

impl Published {
    pub fn queue(&self) -> u16 {
        self.queue
    }
}

#[derive(Clone, Copy, Default)]
struct Slot {
    /// Part of a chain in flight.
    held: bool,
    /// At a head: how many descriptors its chain is. 0 anywhere else.
    descs: u16,
    /// At a head: bytes its device-writable elements hold.
    writable: u32,
}

/// One split virtqueue over the grant `M`.
pub struct Virtqueue<M: DmaBuffers> {
    mem: M,
    index: u16,
    size: u16,
    parts: Parts,
    /// The available index, as published. Never read back from the ring.
    next_avail: u16,
    /// How many used elements have been taken, as the ring counts them.
    last_used: u16,
    /// One per descriptor: the table's own length is the bound a used
    /// element's head is held to.
    slots: Vec<Slot>,
}

impl<M: DmaBuffers> Virtqueue<M> {
    /// Queue `index` of `size` descriptors, its parts where `parts` says, all
    /// three zeroed (§4.1.5.1.3 step 4, and §2.7.10.1 of `used.flags`).
    ///
    /// # Panics
    /// The layout is the caller's own, so one the specification refuses is its
    /// mistake: a size that is no power of two or past [`MAX_QUEUE_SIZE`]
    /// (§2.7, §4.1.4.3.2), a part past the grant, or one off §2.7.1's
    /// alignment, here or at the device.
    pub fn new(mem: M, index: u16, size: u16, parts: Parts) -> Self {
        assert!(
            size.is_power_of_two() && size <= MAX_QUEUE_SIZE,
            "virtqueue {index}: {size} descriptors is no queue size"
        );
        for (what, at, bytes, align) in [
            ("descriptor table", parts.desc, desc_bytes(size), 16),
            ("available ring", parts.avail, avail_bytes(size), 2),
            ("used ring", parts.used, used_bytes(size), 4),
        ] {
            assert!(
                at.checked_add(bytes).is_some_and(|end| end <= mem.bytes()),
                "virtqueue {index}: its {what} at {at:#x} runs past a grant of {:#x} bytes",
                mem.bytes()
            );
            assert!(
                at % align == 0 && mem.device_addr(at) % align as u64 == 0,
                "virtqueue {index}: its {what} at {at:#x} is not on a multiple of {align}"
            );
        }
        for at in (0..desc_bytes(size)).step_by(8) {
            mem.write64(parts.desc + at, 0);
        }
        for at in (0..avail_bytes(size)).step_by(2) {
            mem.write16(parts.avail + at, 0);
        }
        for at in (0..used_bytes(size)).step_by(2) {
            mem.write16(parts.used + at, 0);
        }
        Self {
            mem,
            index,
            size,
            parts,
            next_avail: 0,
            last_used: 0,
            slots: vec![Slot::default(); size as usize],
        }
    }

    pub fn index(&self) -> u16 {
        self.index
    }

    pub fn size(&self) -> u16 {
        self.size
    }

    /// What the device is told for each part (§4.1.4.3's `queue_desc`,
    /// `queue_driver` and `queue_device`).
    pub fn desc_addr(&self) -> u64 {
        self.mem.device_addr(self.parts.desc)
    }

    pub fn avail_addr(&self) -> u64 {
        self.mem.device_addr(self.parts.avail)
    }

    pub fn used_addr(&self) -> u64 {
        self.mem.device_addr(self.parts.used)
    }

    /// Make `chain` available at `head` and the descriptors after it.
    ///
    /// # Panics
    /// The chain is the caller's own, so one the specification refuses is its
    /// mistake: an empty one, one past the table, one over a descriptor still
    /// in flight, a device-readable element after a device-writable one
    /// (§2.7.4.2), or 2^32 bytes and more (§2.7.5.2).
    pub fn publish(&mut self, head: u16, chain: &[Buffer]) -> Published {
        let (queue, size) = (self.index, self.size);
        let first = head as usize;
        assert!(!chain.is_empty(), "virtqueue {queue}: a chain of no buffers at head {head}");
        assert!(
            first + chain.len() <= size as usize,
            "virtqueue {queue}: a chain of {} at head {head} runs past a table of {size}",
            chain.len()
        );
        let mut total = 0u32;
        let mut writable = 0u32;
        for (nth, buffer) in chain.iter().enumerate() {
            assert!(
                !self.slots[first + nth].held,
                "virtqueue {queue}: a chain at head {head} takes descriptor {}, which is in flight",
                first + nth
            );
            assert!(
                buffer.writable || writable == 0,
                "virtqueue {queue}: the chain at head {head} has a device-readable element after \
                 a device-writable one"
            );
            total = total.checked_add(buffer.len).unwrap_or_else(|| {
                panic!("virtqueue {queue}: the chain at head {head} is 2^32 bytes or more")
            });
            if buffer.writable {
                writable += buffer.len;
            }
        }

        for (nth, buffer) in chain.iter().enumerate() {
            let last = nth + 1 == chain.len();
            let at = self.parts.desc + (first + nth) * DESC_BYTES;
            let direction = if buffer.writable { VIRTQ_DESC_F_WRITE } else { 0 };
            self.mem.write64(at, buffer.addr);
            self.mem.write32(at + DESC_LEN, buffer.len);
            self.mem.write16(at + DESC_FLAGS, direction | if last { 0 } else { VIRTQ_DESC_F_NEXT });
            // In range: `first + nth + 1 < first + chain.len() <= size`.
            self.mem.write16(at + DESC_NEXT, if last { 0 } else { (first + nth + 1) as u16 });
            self.slots[first + nth].held = true;
        }
        self.slots[first].descs = chain.len() as u16;
        self.slots[first].writable = writable;

        let entry = (self.next_avail % size) as usize;
        self.mem.write16(self.parts.avail + RING_ENTRIES + entry * AVAIL_ENTRY_BYTES, head);
        self.mem.publish();
        self.next_avail = self.next_avail.wrapping_add(1);
        self.mem.write16(self.parts.avail + RING_IDX, self.next_avail);
        self.mem.publish();
        Published { queue }
    }

    /// The next chain the device has finished with, `None` when the ring holds
    /// no more, or why what the ring holds was not believed.
    ///
    /// A chain is retired as it is answered, so a head the device reports
    /// twice is refused the second time.
    pub fn poll_used(&mut self) -> Result<Option<Used>, UsedRefusal> {
        let used: u16 = self.mem.read16(self.parts.used + RING_IDX);
        let pending = used.wrapping_sub(self.last_used);
        if pending == 0 {
            return Ok(None);
        }
        // §2.7.8: an element "matches an entry placed in the available ring by
        // the guest earlier", so the device's count never passes the driver's.
        let available = self.next_avail.wrapping_sub(self.last_used);
        if pending > available {
            return Err(UsedRefusal::Jumped { used, taken: self.last_used, available: self.next_avail });
        }
        self.mem.observe();
        let at = self.parts.used
            + RING_ENTRIES
            + (self.last_used % self.size) as usize * USED_ELEM_BYTES;
        let id = Untrusted::new(self.mem.read32(at));
        let len = Untrusted::new(self.mem.read32(at + USED_ELEM_LEN));
        self.last_used = self.last_used.wrapping_add(1);

        let first = id.index(self.slots.len()).map_err(UsedRefusal::Head)?;
        // Exact: `index` proved it below the table's length, a `u16`.
        let head = first as u16;
        let chain = self.slots[first];
        if chain.descs == 0 {
            return Err(UsedRefusal::NoChain { head });
        }
        let written = len
            .at_most(chain.writable as u64)
            .map_err(|len| UsedRefusal::Written { head, len })?;
        for slot in &mut self.slots[first..first + chain.descs as usize] {
            *slot = Slot::default();
        }
        // Exact: `at_most` proved it no more than `chain.writable`, a `u32`.
        Ok(Some(Used { head, written: written as u32 }))
    }
}
