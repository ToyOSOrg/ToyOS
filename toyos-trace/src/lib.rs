//! The diary's decoder: what a `toyos_abi::trace::TraceRecord` says, typed,
//! when it said it, and the one line it prints as.
//!
//! A record crossed the syscall boundary or came off a file, so it is input:
//! [`Entry::decode`] refuses a kind the ABI does not name, and a kind that
//! names its own task but carries no thread, each by name. A stamp is the
//! writing CPU's counter, which the machine's `ClockPage` reads as
//! nanoseconds since boot on the kernel's own formula, so a diary and a log
//! line order against each other.
//!
//! Pure: `core` only, no `unsafe`, no I/O.

#![no_std]
#![forbid(unsafe_code)]

use core::fmt;

use toyos_abi::clock::{nanos_between, ClockPage};
use toyos_abi::trace::{Kind, TraceRecord, NO_THREAD};

/// A thread, by its process's id and its own, each as the writing CPU held
/// it: [`NO_THREAD`] where it held none.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Thread {
    pub pid: u32,
    pub tid: u32,
}

/// What happened, with what the kind says of `data`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Pick,
    Wake,
    Park,
    Preempt,
    Migrate { to: u32 },
    Adopt,
    Retire,
    IdleEnter,
    TimerArm { span_ns: u32 },
    TimerStop,
    TimerFire,
    TimerFireBurst { fires: u32 },
    IrqDrain { source: u8, latency_us: u32 },
    Mark { index: u32 },
}

impl Event {
    /// Whether the record's thread is the task the event happened to, rather
    /// than whichever thread the CPU was running.
    pub const fn names_its_task(&self) -> bool {
        matches!(self, Self::Pick | Self::Wake | Self::Park | Self::Migrate { .. } | Self::Adopt | Self::Retire)
    }

    const fn name(&self) -> &'static str {
        match self {
            Self::Pick => "pick",
            Self::Wake => "wake",
            Self::Park => "park",
            Self::Preempt => "preempt",
            Self::Migrate { .. } => "migrate",
            Self::Adopt => "adopt",
            Self::Retire => "retire",
            Self::IdleEnter => "idle",
            Self::TimerArm { .. } => "timer-arm",
            Self::TimerStop => "timer-stop",
            Self::TimerFire => "timer-fire",
            Self::TimerFireBurst { .. } => "timer-fires",
            Self::IrqDrain { .. } => "irq-drain",
            Self::Mark { .. } => "mark",
        }
    }
}

/// One record, decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Its number in its CPU's ring.
    pub seq: u64,
    /// The writing CPU's counter.
    pub stamp: u64,
    pub cpu: u16,
    /// The task, where [`Event::names_its_task`]; otherwise the thread the
    /// CPU was running, `None` for none.
    pub thread: Option<Thread>,
    pub event: Event,
}

/// Why a record is not an [`Entry`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Undecodable {
    /// A kind word the ABI does not name.
    Kind(u16),
    /// A kind that names its own task, with no thread.
    NoTask(u16),
}

impl fmt::Display for Undecodable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Kind(word) => write!(f, "kind {word} is no kind the ABI names"),
            Self::NoTask(word) => write!(f, "kind {word} names its task and carries no thread"),
        }
    }
}

impl Entry {
    pub fn decode(raw: &TraceRecord) -> Result<Self, Undecodable> {
        let kind = Kind::from_u16(raw.kind).ok_or(Undecodable::Kind(raw.kind))?;
        let event = match kind {
            Kind::Pick => Event::Pick,
            Kind::Wake => Event::Wake,
            Kind::ParkCommit => Event::Park,
            Kind::Preempt => Event::Preempt,
            Kind::Migrate => Event::Migrate { to: raw.data },
            Kind::Adopt => Event::Adopt,
            Kind::Retire => Event::Retire,
            Kind::IdleEnter => Event::IdleEnter,
            Kind::TimerArm => Event::TimerArm { span_ns: raw.data },
            Kind::TimerStop => Event::TimerStop,
            Kind::TimerFire => Event::TimerFire,
            Kind::TimerFireBurst => Event::TimerFireBurst { fires: raw.data },
            Kind::IrqDrain => Event::IrqDrain { source: (raw.data >> 24) as u8, latency_us: raw.data & 0x00FF_FFFF },
            Kind::Mark => Event::Mark { index: raw.data },
        };
        let thread = (raw.pid != NO_THREAD || raw.tid != NO_THREAD).then_some(Thread { pid: raw.pid, tid: raw.tid });
        if event.names_its_task() && (raw.pid == NO_THREAD || raw.tid == NO_THREAD) {
            return Err(Undecodable::NoTask(raw.kind));
        }
        Ok(Self { seq: raw.seq, stamp: raw.stamp, cpu: raw.cpu, thread, event })
    }

    /// The line this entry prints as, its stamp read on `clock`:
    /// `[12.000345678 cpu3] wake 7/0`.
    pub fn line(&self, clock: ClockPage) -> Line<'_> {
        Line { entry: self, clock }
    }
}

/// What [`Entry::line`] renders.
pub struct Line<'a> {
    entry: &'a Entry,
    clock: ClockPage,
}

impl fmt::Display for Line<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let e = self.entry;
        let nanos = nanos_between(self.clock.counter_at_boot, self.clock.period_fs, e.stamp);
        write!(f, "[{}.{:09} cpu{}] {}", nanos / 1_000_000_000, nanos % 1_000_000_000, e.cpu, e.event.name())?;
        match e.thread {
            Some(t) => write!(f, " {}/{}", Id(t.pid), Id(t.tid))?,
            None => f.write_str(" -")?,
        }
        match e.event {
            Event::Migrate { to } => write!(f, " to=cpu{to}"),
            Event::TimerArm { span_ns } => write!(f, " in={span_ns}ns"),
            Event::TimerFireBurst { fires } => write!(f, " fires={fires}"),
            Event::IrqDrain { source, latency_us } => write!(f, " source={source} after={latency_us}us"),
            Event::Mark { index } => write!(f, " index={index}"),
            Event::Pick
            | Event::Wake
            | Event::Park
            | Event::Preempt
            | Event::Adopt
            | Event::Retire
            | Event::IdleEnter
            | Event::TimerStop
            | Event::TimerFire => Ok(()),
        }
    }
}

/// A pid or tid, `-` where the record has none.
struct Id(u32);

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            NO_THREAD => f.write_str("-"),
            id => write!(f, "{id}"),
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use std::string::ToString;

    use super::*;

    /// A 1 GHz counter that read 1000 at boot: one tick a nanosecond.
    const CLOCK: ClockPage =
        ClockPage { magic: toyos_abi::clock::CLOCK_MAGIC, counter_at_boot: 1_000, period_fs: 1_000_000, stamp_at_boot: 0 };

    fn raw(kind: Kind, data: u32, pid: u32, tid: u32) -> TraceRecord {
        TraceRecord { seq: 9, stamp: 1_000 + 12_000_345_678, kind: kind as u16, cpu: 3, data, pid, tid }
    }

    #[test]
    fn a_wake_names_its_task_and_prints_when_on_which_cpu() {
        let entry = Entry::decode(&raw(Kind::Wake, 0, 7, 0)).unwrap();
        assert_eq!(entry.thread, Some(Thread { pid: 7, tid: 0 }));
        assert_eq!(entry.event, Event::Wake);
        assert_eq!(entry.line(CLOCK).to_string(), "[12.000345678 cpu3] wake 7/0");
    }

    /// Thread zero is a thread: the first of every process.
    #[test]
    fn thread_zero_is_a_thread_and_no_thread_is_said() {
        let on_none = Entry::decode(&raw(Kind::IdleEnter, 0, NO_THREAD, NO_THREAD)).unwrap();
        assert_eq!(on_none.thread, None);
        assert_eq!(on_none.line(CLOCK).to_string(), "[12.000345678 cpu3] idle -");
        let half = Entry::decode(&raw(Kind::TimerFire, 0, NO_THREAD, 4)).unwrap();
        assert_eq!(half.line(CLOCK).to_string(), "[12.000345678 cpu3] timer-fire -/4");
    }

    #[test]
    fn every_kind_decodes_with_what_its_data_says() {
        let cases = [
            (Kind::Pick, 0, Event::Pick, ""),
            (Kind::ParkCommit, 0, Event::Park, ""),
            (Kind::Preempt, 0, Event::Preempt, ""),
            (Kind::Migrate, 5, Event::Migrate { to: 5 }, " to=cpu5"),
            (Kind::Adopt, 0, Event::Adopt, ""),
            (Kind::Retire, 0, Event::Retire, ""),
            (Kind::TimerArm, 250_000, Event::TimerArm { span_ns: 250_000 }, " in=250000ns"),
            (Kind::TimerStop, 0, Event::TimerStop, ""),
            (Kind::TimerFireBurst, 3, Event::TimerFireBurst { fires: 3 }, " fires=3"),
            (Kind::IrqDrain, 2 << 24 | 13, Event::IrqDrain { source: 2, latency_us: 13 }, " source=2 after=13us"),
            (Kind::Mark, 41, Event::Mark { index: 41 }, " index=41"),
        ];
        for (kind, data, event, tail) in cases {
            let entry = Entry::decode(&raw(kind, data, 1, 2)).unwrap();
            assert_eq!(entry.event, event, "{kind:?}");
            assert!(entry.line(CLOCK).to_string().ends_with(&std::format!(" 1/2{tail}")), "{kind:?}");
        }
    }

    #[test]
    fn a_kind_the_abi_does_not_name_is_refused_by_its_number() {
        let mut r = raw(Kind::Pick, 0, 1, 2);
        r.kind = 0;
        assert_eq!(Entry::decode(&r), Err(Undecodable::Kind(0)));
        r.kind = 15;
        assert_eq!(Entry::decode(&r), Err(Undecodable::Kind(15)));
    }

    #[test]
    fn a_task_event_with_no_task_is_refused() {
        assert_eq!(Entry::decode(&raw(Kind::Pick, 0, 1, NO_THREAD)), Err(Undecodable::NoTask(Kind::Pick as u16)));
        assert_eq!(Entry::decode(&raw(Kind::Wake, 0, NO_THREAD, 0)), Err(Undecodable::NoTask(Kind::Wake as u16)));
    }

}
