//! A program's log ring as it lies in memory: one 2 MiB shared region — a
//! header, [`LANES`] lanes of [`LANE_SLOTS`] slots, and [`SHARED_SLOTS`] slots
//! of shared ring — each slot a protocol word and a [`Body`].
//!
//! `/system/bin/init` creates one per program it starts, lays it out before
//! any other process can map it ([`Ring::lay_out`]), gives the program
//! duplicates as its stdout and stderr, and hands `/system/bin/logd` the
//! region with the program's name. The program writes, `logd` reads, and
//! [`super::ring`] is the whole of what they agree on beyond this layout.
//!
//! **Every access goes through the protocol's words or a volatile copy of a
//! whole body**: the region is shared with another process, so no Rust
//! reference to anything in it but an atomic word is ever formed.

use core::ptr::NonNull;
use core::sync::atomic::{AtomicU64, Ordering};

use super::ring::{self, Pushed, Shared, Slots};
use toyos_abi::log::Severity;

/// The region's length: the kernel's shared-memory granule.
pub const RING_BYTES: usize = 2 * 1024 * 1024;

/// One slot. The header takes the first.
pub const SLOT_BYTES: usize = 1024;

/// Lanes a ring has, for threads that may not retry a write.
pub const LANES: usize = 4;

/// Slots in one lane.
pub const LANE_SLOTS: u64 = 32;

/// Slots in the shared ring: everything past the header and the lanes.
pub const SHARED_SLOTS: u64 = (RING_BYTES / SLOT_BYTES) as u64 - 1 - LANES as u64 * LANE_SLOTS;

/// The protocol word that opens every slot, and the word beside it.
const SLOT_WORDS: usize = 16;

/// A record's fixed fields, ahead of its text.
const BODY_HEAD: usize = 24;

/// Text one record carries. A longer line is several records, each but the
/// last marked [`FLAG_UNENDED`].
pub const TEXT_BYTES: usize = SLOT_BYTES - SLOT_WORDS - BODY_HEAD;

/// What a laid-out region opens with.
pub const MAGIC: u64 = 0x544F_594F_534C_4F47; // "TOYOSLOG"

/// The header's words, each group a cache line of its own: writers hammer
/// `head` and `refused`, the reader `tail`.
const MAGIC_AT: usize = 0;
/// The process init started with the ring, as `logd` registered it: zero
/// until then.
const OWNER_AT: usize = 8;
const HEAD_AT: usize = 64;
const TAIL_AT: usize = 128;
const REFUSED_AT: usize = 192;
/// Lane `i`'s writer line — owner, head, refused — and its reader line.
const LANE_AT: usize = 256;
const LANE_BYTES: usize = 128;
const LANE_OWNER: usize = 0;
const LANE_HEAD: usize = 8;
const LANE_REFUSED: usize = 16;
const LANE_TAIL: usize = 64;

const _: () = assert!(LANE_AT + LANES * LANE_BYTES <= SLOT_BYTES);

/// A claimed lane's owner word: the claim bit, the process and the thread.
const CLAIMED: u64 = 1 << 63;

/// Shared slots a writer other than the ring's owner leaves free: a child
/// that fills its parent's ring does not take the parent's own next lines
/// with it.
pub const CHILD_KEEP: u64 = 64;

/// The writer's line continues in its next record.
pub const FLAG_UNENDED: u8 = 1 << 0;
/// The record ends a line its writer's earlier records began, and carries
/// nothing of its own: where no line is open it is not one.
pub const FLAG_CLOSES: u8 = 1 << 1;

/// One record: stamped by its writer at the moment it wrote it.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Body {
    /// Nanoseconds since boot, on the clock the kernel's records carry.
    pub at_ns: u64,
    /// The writing process and thread, as the writer states them: its own
    /// word, inside the identity the ring already has.
    pub pid: u32,
    pub tid: u32,
    /// Text bytes present; a reader clamps it to [`TEXT_BYTES`].
    pub len: u16,
    pub severity: u8,
    pub flags: u8,
    pub _pad: u32,
    pub text: [u8; TEXT_BYTES],
}

const _: () = assert!(core::mem::size_of::<Body>() == BODY_HEAD + TEXT_BYTES);
const _: () = assert!(SLOT_WORDS + core::mem::size_of::<Body>() == SLOT_BYTES);

impl Body {
    pub const EMPTY: Self = Self {
        at_ns: 0,
        pid: 0,
        tid: 0,
        len: 0,
        severity: Severity::Info as u8,
        flags: 0,
        _pad: 0,
        text: [0; TEXT_BYTES],
    };

    /// The text, clamped: the writer's `len` is its word, not a bound.
    pub fn text(&self) -> &[u8] {
        &self.text[..(self.len as usize).min(TEXT_BYTES)]
    }

    /// `None` is a severity no writer of this ABI produces.
    pub fn severity(&self) -> Option<Severity> {
        Severity::from_u8(self.severity)
    }

    pub fn unended(&self) -> bool {
        self.flags & FLAG_UNENDED != 0
    }

    pub fn closes(&self) -> bool {
        self.flags & FLAG_CLOSES != 0
    }
}

impl ring::Word for AtomicU64 {
    fn load(&self, order: Ordering) -> u64 {
        AtomicU64::load(self, order)
    }
    fn store(&self, value: u64, order: Ordering) {
        AtomicU64::store(self, value, order)
    }
    fn fetch_add(&self, value: u64, order: Ordering) -> u64 {
        AtomicU64::fetch_add(self, value, order)
    }
    fn compare_exchange_weak(
        &self,
        current: u64,
        new: u64,
        success: Ordering,
        failure: Ordering,
    ) -> Result<u64, u64> {
        AtomicU64::compare_exchange_weak(self, current, new, success, failure)
    }
}

/// A view of one mapped ring. It owns nothing: whoever mapped the region keeps
/// it mapped for as long as this is used.
#[derive(Clone, Copy)]
pub struct Ring {
    base: NonNull<u8>,
}

// SAFETY: every access through a `Ring` is an atomic word or a volatile copy
// the protocol makes exclusive, so sharing the view between threads shares
// nothing unsynchronised.
unsafe impl Send for Ring {}
// SAFETY: as for `Send`.
unsafe impl Sync for Ring {}

impl Ring {
    /// # Safety
    /// `base` is the start of a mapping at least [`RING_BYTES`] long, 64-byte
    /// aligned, that stays mapped for as long as this view or a copy of it is
    /// used.
    pub unsafe fn at(base: NonNull<u8>) -> Self {
        Self { base }
    }

    /// Write the header into a region nobody else has mapped yet. Every other
    /// word is zero in a fresh region, and zero is the empty state of each.
    pub fn lay_out(&self) {
        // SAFETY: the header lies inside the mapping `at` was given.
        unsafe { core::ptr::write_volatile(self.at_offset(MAGIC_AT) as *mut u64, MAGIC) };
    }

    /// Whether the region was laid out as a ring.
    pub fn is_laid_out(&self) -> bool {
        // SAFETY: as for `lay_out`.
        unsafe { core::ptr::read_volatile(self.at_offset(MAGIC_AT) as *const u64) == MAGIC }
    }

    /// Name the process the ring is its log of, which [`Self::push`] keeps
    /// the ring's last slots for. The reader's to say, once it knows.
    pub fn own(&self, pid: u32) {
        self.word(OWNER_AT).store(u64::from(pid), Ordering::Relaxed);
    }

    /// Write one record into the shared ring, leaving [`CHILD_KEEP`] slots
    /// where the writer is not the ring's owner.
    pub fn push(&self, body: &Body) -> Pushed {
        let owner = self.word(OWNER_AT).load(Ordering::Relaxed);
        let keep = if owner == 0 || owner == u64::from(body.pid) { 0 } else { CHILD_KEEP };
        ring::push_leaving(self, body, keep)
    }

    /// Lane `index`, to read.
    pub fn lane(&self, index: usize) -> Lane {
        assert!(index < LANES, "log ring: lane {index} of {LANES}");
        Lane { ring: *self, index }
    }

    /// Claim a free lane for the thread `tid` of process `pid`, which becomes
    /// its only writer; `None` when every lane is claimed. A claim is for the
    /// ring's life.
    pub fn claim_lane(&self, pid: u32, tid: u32) -> Option<Lane> {
        let owner = CLAIMED | (pid as u64) << 32 | tid as u64;
        (0..LANES).map(|index| self.lane(index)).find(|lane| {
            lane.word(LANE_OWNER).compare_exchange(0, owner, Ordering::Relaxed, Ordering::Relaxed).is_ok()
        })
    }

    fn at_offset(&self, offset: usize) -> *mut u8 {
        assert!(offset < RING_BYTES, "log ring: offset {offset:#x} past the region");
        // SAFETY: inside the mapping, by the assertion.
        unsafe { self.base.as_ptr().add(offset) }
    }

    fn word(&self, offset: usize) -> &AtomicU64 {
        // SAFETY: an 8-aligned offset inside the mapping; the word is only
        // ever accessed atomically, by every party.
        unsafe { &*(self.at_offset(offset) as *const AtomicU64) }
    }

    fn write_body(&self, slot: usize, body: &Body) {
        let at = self.at_offset(SLOT_BYTES * slot + SLOT_WORDS) as *mut Body;
        // SAFETY: inside the mapping, 8-aligned, and the protocol makes this
        // writer the slot's only accessor until it publishes.
        unsafe { core::ptr::write_volatile(at, *body) };
    }

    fn read_body(&self, slot: usize) -> Body {
        let at = self.at_offset(SLOT_BYTES * slot + SLOT_WORDS) as *const Body;
        // SAFETY: as for `write_body`, for the reader after the publication.
        unsafe { core::ptr::read_volatile(at) }
    }

    /// The region slot of the shared ring's slot `slot`.
    fn shared_slot(slot: usize) -> usize {
        1 + LANES * LANE_SLOTS as usize + slot
    }
}

impl Slots for Ring {
    type Word = AtomicU64;
    type Body = Body;

    fn slots(&self) -> u64 {
        SHARED_SLOTS
    }
    fn head(&self) -> &AtomicU64 {
        self.word(HEAD_AT)
    }
    fn tail(&self) -> &AtomicU64 {
        self.word(TAIL_AT)
    }
    fn refused(&self) -> &AtomicU64 {
        self.word(REFUSED_AT)
    }
    fn write(&self, slot: usize, body: &Body) {
        self.write_body(Self::shared_slot(slot), body)
    }
    fn read(&self, slot: usize) -> Body {
        self.read_body(Self::shared_slot(slot))
    }
}

impl Shared for Ring {
    fn published(&self, slot: usize) -> &AtomicU64 {
        self.word(SLOT_BYTES * Self::shared_slot(slot))
    }
}

/// One lane of a ring: a writer that claimed it pushes, the reader reads.
#[derive(Clone, Copy)]
pub struct Lane {
    ring: Ring,
    index: usize,
}

impl Lane {
    /// Write one record. Wait-free, and only for the thread that claimed it.
    pub fn push(&self, body: &Body) -> Pushed {
        ring::push_lane(self, body)
    }

    fn word(&self, offset: usize) -> &AtomicU64 {
        self.ring.word(LANE_AT + self.index * LANE_BYTES + offset)
    }

    fn region_slot(&self, slot: usize) -> usize {
        1 + self.index * LANE_SLOTS as usize + slot
    }
}

impl Slots for Lane {
    type Word = AtomicU64;
    type Body = Body;

    fn slots(&self) -> u64 {
        LANE_SLOTS
    }
    fn head(&self) -> &AtomicU64 {
        self.word(LANE_HEAD)
    }
    fn tail(&self) -> &AtomicU64 {
        self.word(LANE_TAIL)
    }
    fn refused(&self) -> &AtomicU64 {
        self.word(LANE_REFUSED)
    }
    fn write(&self, slot: usize, body: &Body) {
        self.ring.write_body(self.region_slot(slot), body)
    }
    fn read(&self, slot: usize) -> Body {
        self.ring.read_body(self.region_slot(slot))
    }
}
