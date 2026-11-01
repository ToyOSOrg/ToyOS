//! What [`SYS_TRACE_READ`] answers: the kernel's diary of scheduling and timer
//! events, one [`TraceRecord`] each, from a ring per CPU the kernel writes
//! always.
//!
//! A record's [`TraceRecord::stamp`] is the CPU's free-running counter (TSC,
//! `CNTVCT_EL0`) in its own ticks, which a [`crate::clock::Clock`] turns into
//! nanoseconds since boot. A read takes a cursor laid out as the log's
//! ([`crate::log::CursorHead`]), a position per CPU the firmware names, and
//! answers records oldest first, merged across CPUs by stamp, with the
//! records overwritten before the cursor reached them counted in its `lost`.
//! What each [`Kind`] means of `pid`, `tid` and `data` is on the kind; the
//! typed reading of one is `toyos-trace`'s.
//!
//! [`SYS_TRACE_READ`]: crate::syscall::SYS_TRACE_READ

/// One record on the wire. A reader indexes by this stride.
pub const RECORD_BYTES: usize = 32;

/// What `pid` and `tid` read when the CPU was running no thread.
pub const NO_THREAD: u32 = u32::MAX;

crate::user_safe! {
    /// One diary record, as a read copies it out.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct TraceRecord {
        /// Its sequence number in its CPU's ring, which a cursor's positions count in.
        pub seq: u64,
        /// The CPU's counter when the record was written.
        pub stamp: u64,
        /// A [`Kind`], undecoded: input until [`Kind::from_u16`] says otherwise.
        pub kind: u16,
        /// The CPU whose ring holds it, which is the CPU that wrote it.
        pub cpu: u16,
        pub data: u32,
        pub pid: u32,
        pub tid: u32,
    }
}

const _: () = assert!(core::mem::size_of::<TraceRecord>() == RECORD_BYTES);

impl TraceRecord {
    /// A record no ring wrote, for sizing a read buffer: sequence numbers
    /// start at one, so this is no record.
    pub const EMPTY: Self = Self { seq: 0, stamp: 0, kind: 0, cpu: 0, data: 0, pid: 0, tid: 0 };
}

/// What a record says happened. `pid`/`tid` name the thread running on the
/// CPU when it was written, [`NO_THREAD`] when none was, unless the kind
/// names its own task; `data` is 0 unless the kind says otherwise.
#[repr(u16)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The task `pid`/`tid` name was picked to run.
    Pick = 1,
    /// The task `pid`/`tid` name was made runnable on this CPU.
    Wake = 2,
    /// The task `pid`/`tid` name parked.
    ParkCommit = 3,
    /// A pass ran on the way out of an interrupt.
    Preempt = 4,
    /// The task `pid`/`tid` name left for the CPU `data` names.
    Migrate = 5,
    /// The task `pid`/`tid` name arrived from another CPU.
    Adopt = 6,
    /// The task `pid`/`tid` name was retired.
    Retire = 7,
    /// The CPU is about to halt.
    IdleEnter = 8,
    /// The one-shot timer was armed `data` nanoseconds out, the low 32 bits of the span.
    TimerArm = 9,
    TimerStop = 10,
    TimerFire = 11,
    /// `data` timer expiries taken in Ring 0 since the last return to Ring 3.
    TimerFireBurst = 12,
    // 13 was `IrqDrain`, the kernel's own drivers' interrupt records.
    /// Written only by a test kernel's flood; `data` is its index.
    Mark = 14,
}

impl Kind {
    /// The kind a record's word names, or `None` for one this ABI does not.
    pub const fn from_u16(word: u16) -> Option<Self> {
        Some(match word {
            1 => Self::Pick,
            2 => Self::Wake,
            3 => Self::ParkCommit,
            4 => Self::Preempt,
            5 => Self::Migrate,
            6 => Self::Adopt,
            7 => Self::Retire,
            8 => Self::IdleEnter,
            9 => Self::TimerArm,
            10 => Self::TimerStop,
            11 => Self::TimerFire,
            12 => Self::TimerFireBurst,
            14 => Self::Mark,
            _ => return None,
        })
    }
}
