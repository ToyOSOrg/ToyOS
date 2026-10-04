//! One CPU's ring of whole records: writers and readers never observe a torn record.
//!
//! [`Ring`] is the slot protocol, over any number of body words whose first is
//! the record's stamp; the log's [`Shard`] and `crate::trace`'s rings are both
//! one. A zeroed ring is an empty one, so a ring needs no constructor run on
//! zeroed storage and a `static` of one is `.bss`.
//! Compiled a second time by `kernel-loom`, which shims only the atomics and `arch::percpu_fetch_add`.

#[cfg(not(feature = "loom"))]
use core::sync::atomic::{fence, AtomicBool, AtomicU64, Ordering};

#[cfg(feature = "loom")]
use loom::sync::atomic::{fence, AtomicBool, AtomicU64, Ordering};

use toyos_abi::log::{LogRecord, MAX_RECORD_MESSAGE};
#[cfg(not(feature = "loom"))]
use toyos_abi::log::RECORD_BYTES;

/// Slots per CPU: 512 KiB at `RECORD_BYTES` of 1024, sized to outlast records emitted before a reader attaches.
#[cfg(not(feature = "loom"))]
pub const SHARD_RECORDS: usize = 512;

/// Small under loom so the recycle model explores a bounded state space and terminates.
#[cfg(feature = "loom")]
pub const SHARD_RECORDS: usize = 4;

/// Packed by hand rather than transmuted, so the layout is this file's agreement and not the compiler's.
const HEADER_WORDS: usize = 3;

/// Message words a slot holds; small under loom so a model shard's atomics fit its stack.
#[cfg(not(feature = "loom"))]
const MSG_WORDS: usize = MAX_RECORD_MESSAGE / 8;
#[cfg(feature = "loom")]
const MSG_WORDS: usize = 1;

/// The whole body: everything in a [`LogRecord`] past the word the writer publishes with.
const BODY_WORDS: usize = HEADER_WORDS + MSG_WORDS;

/// The message bound this file enforces: the ABI's in the kernel build, the model's own under loom.
const MSG_BYTES: usize = MSG_WORDS * 8;

#[cfg(not(feature = "loom"))]
const _: () = assert!(MSG_BYTES == MAX_RECORD_MESSAGE);
#[cfg(not(feature = "loom"))]
const _: () = assert!(BODY_WORDS * 8 == RECORD_BYTES - core::mem::size_of::<u64>());

// Must stay `Release`, so every body word precedes this store for an acquiring reader; `log-commit-release-off` flips it so `kernel-loom` can prove that.
#[cfg(not(feature = "log-commit-release-off"))]
const PUBLISH: Ordering = Ordering::Release;
#[cfg(feature = "log-commit-release-off")]
const PUBLISH: Ordering = Ordering::Relaxed;

/// The three identity words, in the order a slot holds them.
fn header(record: &LogRecord, len: u16) -> [u64; HEADER_WORDS] {
    [
        record.at_ns,
        record.pid as u64 | (record.tid as u64) << 32,
        record.cpu as u64
            | (len as u64) << 16
            | (record.elided as u64) << 32
            | (record.severity as u64) << 48
            | (record.flags as u64) << 56,
    ]
}

/// How many message words a record of this length occupies.
fn msg_words(len: u16) -> usize {
    (len as usize).min(MSG_BYTES).div_ceil(8)
}

/// First sequence number any ring issues; never 0, since a zeroed slot reads as sequence 0.
pub const FIRST_SEQ: u64 = 1;

// A slot mid-write, holding no record a reader may accept; `u64::MAX` is unreachable as a real sequence number.
// The only state this word carries, not a separate flag that could disagree with it.
const WRITING: u64 = u64::MAX;

/// One record's storage: every word is atomic because a reader may load while a writer stores.
#[repr(C)]
pub struct Slot<const W: usize> {
    /// State word: a sequence number, [`WRITING`], or zero for untouched.
    seq: AtomicU64,
    /// The record's words; the first is its stamp, which a merge across rings orders by.
    body: [AtomicU64; W],
}

/// The ring's reservation count alone on its cache line, so the slots after it start on one.
#[repr(C, align(64))]
struct Issued(AtomicU64);

/// One CPU's records, `N` slots of `W` body words.
#[repr(C, align(64))]
pub struct Ring<const W: usize, const N: usize> {
    /// Sequence numbers this ring has handed out, so zero is an empty ring; only the owning CPU writes it.
    issued: Issued,
    slots: [Slot<W>; N],
}

/// The log's ring: a [`LogRecord`] a slot, laid out exactly as one.
pub type Shard = Ring<BODY_WORDS, SHARD_RECORDS>;

// Layout assertions are skipped under loom: its atomics carry tracking state and are wider than the real ones.
#[cfg(not(feature = "loom"))]
const _: () = assert!(core::mem::size_of::<Slot<BODY_WORDS>>() == RECORD_BYTES);
#[cfg(not(feature = "loom"))]
const _: () = assert!(core::mem::offset_of!(Shard, slots) == 64);
const _: () = assert!(core::mem::offset_of!(LogRecord, at_ns) == core::mem::size_of::<u64>());

impl<const W: usize, const N: usize> Ring<W, N> {
    /// An empty ring: every word zero.
    #[cfg(not(feature = "loom"))]
    pub const fn new() -> Self {
        Self {
            issued: Issued(AtomicU64::new(0)),
            slots: [const { Slot { seq: AtomicU64::new(0), body: [const { AtomicU64::new(0) }; W] } }; N],
        }
    }

    /// Loom's atomics have no `const` constructor, so this builds rings at run time.
    // No `Default` beside it: the kernel's arm must stay `const`, which `Default::default` cannot be.
    #[allow(clippy::new_without_default)]
    #[cfg(feature = "loom")]
    pub fn new() -> Self {
        Self {
            issued: Issued(AtomicU64::new(0)),
            slots: core::array::from_fn(|_| Slot {
                seq: AtomicU64::new(0),
                body: core::array::from_fn(|_| AtomicU64::new(0)),
            }),
        }
    }

    /// The next sequence number this ring will issue (a reservation count, not a commit count; `Acquire`, paired with the commit store).
    pub fn head(&self) -> u64 {
        self.issued.0.load(Ordering::Acquire) + FIRST_SEQ
    }

    /// Oldest sequence number this ring can still answer for.
    pub fn oldest_readable(&self) -> u64 {
        self.head().saturating_sub(N as u64).max(FIRST_SEQ)
    }

    /// Take the next sequence number, on the CPU that owns this ring.
    /// # Safety
    /// Caller must be the owning CPU, and `guard` must stay live through the matching [`Ring::publish`].
    pub unsafe fn reserve(&self, guard: &crate::arch::IrqGuard) -> u64 {
        crate::arch::percpu_fetch_add(&self.issued.0, guard) + FIRST_SEQ
    }

    /// Write a record's body words into the slot `seq` names through `write`, which stores `Relaxed`, and publish it.
    /// # Safety
    /// `seq` must come from this ring's own [`Ring::reserve`] with the same live `guard`, and this must be the first call for `seq`.
    pub unsafe fn publish(
        &self,
        seq: u64,
        _guard: &crate::arch::IrqGuard,
        write: impl FnOnce(&[AtomicU64; W]),
    ) {
        debug_assert!(
            self.head().saturating_sub(seq) < N as u64,
            "reservation {seq} was lapped inside its publication bracket: an IF-ignoring path emitted a whole ring generation before this commit"
        );

        let slot = &self.slots[(seq % N as u64) as usize];

        // Mark the slot `WRITING` before the body write, so a racing reader's re-check cannot see a false match.
        slot.seq.store(WRITING, Ordering::Relaxed);
        fence(Ordering::Release);

        // Relaxed: the fence above and the release store below order the whole body against the sequence number.
        write(&slot.body);

        // Last store: this publishes the record.
        slot.seq.store(seq, PUBLISH);
    }

    /// What `load` makes of record `seq`'s words, which it loads `Relaxed`, or `None` if this ring cannot answer for it.
    pub fn load<T>(&self, seq: u64, load: impl FnOnce(&[AtomicU64; W]) -> T) -> Option<T> {
        // Both bounds needed: `head` alone counts reservations, so `seq < head` admits a slot not yet committed.
        if seq < self.oldest_readable() || seq >= self.head() {
            return None;
        }
        let slot = &self.slots[(seq % N as u64) as usize];
        // No ABA: slot `j` only ever holds numbers congruent to `j` mod `N`, and `seq` never wraps, so a stale value can't match.
        if slot.seq.load(Ordering::Acquire) != seq {
            return None;
        }

        // A writer may be storing here now; the re-check below discards a torn result.
        let loaded = load(&slot.body);

        // Re-check is total: a writer marks `WRITING` before touching the body, so a match here means nothing wrote in between.
        fence(Ordering::Acquire);
        if slot.seq.load(Ordering::Relaxed) != seq {
            return None;
        }
        Some(loaded)
    }

    /// Stamp of record `seq`, under [`Ring::load`]'s validity test, without copying the rest.
    /// Exists so a merge across rings can hold one candidate each without a whole record per ring.
    pub fn stamp(&self, seq: u64) -> Option<u64> {
        self.load(seq, |body| body[0].load(Ordering::Relaxed))
    }
}

impl Shard {
    /// Write a record's body into the slot `seq` names and publish it.
    /// # Safety
    /// [`Ring::publish`]'s.
    pub unsafe fn commit(&self, seq: u64, record: &LogRecord, guard: &crate::arch::IrqGuard) {
        // SAFETY: the caller's contract is `publish`'s.
        unsafe {
            self.publish(seq, guard, |body| {
                let len = record.len.min(MSG_BYTES as u16);
                for (word, value) in body.iter().zip(header(record, len)) {
                    word.store(value, Ordering::Relaxed);
                }
                for i in 0..msg_words(len) {
                    let mut bytes = [0u8; 8];
                    bytes.copy_from_slice(&record.msg[i * 8..i * 8 + 8]);
                    body[HEADER_WORDS + i].store(u64::from_le_bytes(bytes), Ordering::Relaxed);
                }
            })
        }
    }

    /// Copy record `seq` out, or `None` if this shard cannot answer for it.
    pub fn read(&self, seq: u64) -> Option<LogRecord> {
        self.load(seq, |body| {
            let identity = body[1].load(Ordering::Relaxed);
            let shape = body[2].load(Ordering::Relaxed);
            // `len` may be garbage mid-recycle; clamping keeps the read in bounds.
            let len = ((shape >> 16) as u16).min(MSG_BYTES as u16);
            let mut record = LogRecord {
                seq,
                at_ns: body[0].load(Ordering::Relaxed),
                pid: identity as u32,
                tid: (identity >> 32) as u32,
                cpu: shape as u16,
                len,
                elided: (shape >> 32) as u16,
                severity: (shape >> 48) as u8,
                flags: (shape >> 56) as u8,
                msg: [0; MAX_RECORD_MESSAGE],
            };
            for i in 0..msg_words(len) {
                let bytes = body[HEADER_WORDS + i].load(Ordering::Relaxed).to_le_bytes();
                record.msg[i * 8..i * 8 + 8].copy_from_slice(&bytes);
            }
            record
        })
    }
}

// `Ring` is `Sync` by auto-derivation (every word is `AtomicU64`); no hand-written `unsafe impl` needed.

/// Is a reader parked on this machine's records? Kept as one flag so the producer's fast path avoids a locked read-modify-write.
#[cfg(not(feature = "loom"))]
static LOG_WAITER: AtomicBool = AtomicBool::new(false);

/// The machine's one waiter flag.
#[cfg(not(feature = "loom"))]
pub fn log_waiter() -> &'static AtomicBool {
    &LOG_WAITER
}

#[cfg(feature = "loom")]
pub fn waiter() -> AtomicBool {
    AtomicBool::new(false)
}

/// Read the waiter flag after the guarded commit store; `true` means this caller owns the post.
pub fn signal_after_commit(waiter: &AtomicBool) -> bool {
    // Load-bearing on x86 too: without this fence a committed record can leave a parked reader unposted.
    #[cfg(not(feature = "wake-fence-off"))]
    fence(Ordering::SeqCst);
    if !waiter.load(Ordering::Relaxed) {
        return false;
    }
    waiter.swap(false, Ordering::AcqRel)
}

/// Arm the flag, fence, and re-scan; `true` means do not park.
/// `committed_record_waiting` must test for a commit, not for `head` moving: `head` counts reservations, so that would busy-wait on a writer still mid-publish.
pub fn arm_waiter(waiter: &AtomicBool, committed_record_waiting: impl Fn() -> bool) -> bool {
    waiter.store(true, Ordering::Relaxed);
    #[cfg(not(feature = "wake-fence-off"))]
    fence(Ordering::SeqCst);
    committed_record_waiting()
}
