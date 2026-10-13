//! The ladder of bench measurements, in the order the specification's §9
//! asks and stopped at the first result that makes the next meaningless.
//!
//! - **M0**: every CPU's patch level before any write. One at or above the
//!   patch's means AMD's published bytes cannot exercise a load, and the
//!   ladder stops there rather than force a same-level write.
//! - **Guard**: cpu0 writes MSR0000_008B, which the bench's PPR (55922
//!   §2.1.14.1) makes Error-on-write, so the #GP guard every later write
//!   relies on is shown to catch one before it has to.
//! - **M1**: cpu0 loads the patch from a 16-byte-aligned copy. Anything but
//!   the level reading the header's afterwards stops the ladder: every later
//!   step reads against a load that happened.
//! - **M2**: a copy with a byte of patch data flipped, then one with the
//!   header's level flipped, each on a core nothing has written.
//! - **M3**: cpu0 loads the same patch again.
//! - **M4**: cpu0's sibling thread reads its level before it writes, then
//!   loads.
//! - **M5**: the patch in place in the container (4-byte aligned, not 16),
//!   then a 16-byte-aligned copy straddling a page boundary, each on a core
//!   still below the patch's level.
//! - Every CPU not yet handed the intact patch loads it, so every core runs it.
//! - **M6**: every CPU's level after the others have halted a while.
//!
//! **M9** is in every write: the counter ticks across it, interrupts off.
//!
//! The kernel is the [`Bench`]; the tests fake one.

use alloc::vec::Vec;
use core::fmt;

/// One CPU the ladder may run on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cpu {
    pub id: u32,
    pub apic: u32,
    /// The core it is a thread of: its two threads read one MSR0000_008B.
    pub core: u32,
}

/// Where the patch block sits when its address is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    /// A copy at the start of a page.
    Aligned,
    /// The block where it lies in the container, 4-byte aligned and not 16.
    InPlace,
    /// A 16-byte-aligned copy that crosses a page boundary.
    Straddling,
}

/// The bytes written there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bytes {
    Intact,
    /// One bit of patch data flipped, past the header.
    BodyFlipped,
    /// The low bit of the header's patch level flipped.
    LevelFlipped,
}

impl Bytes {
    /// The patch level the header of these bytes says.
    pub fn header_level(self, level: u32) -> u32 {
        match self {
            Self::LevelFlipped => level ^ 1,
            Self::Intact | Self::BodyFlipped => level,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Staged {
    pub placement: Placement,
    pub bytes: Bytes,
}

const INTACT: Staged = Staged { placement: Placement::Aligned, bytes: Bytes::Intact };

/// One guarded write, as the CPU that made it saw it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Write {
    /// The linear address written, for a loader write.
    pub at: u64,
    /// The level read just before.
    pub before: u32,
    /// The #GP's error code, if the write raised one.
    pub fault: Option<u32>,
    /// The level read after it (and after CPUID leaf 0).
    pub after: u32,
    /// Counter ticks across the write alone, and their nanoseconds.
    pub ticks: u64,
    pub nanos: u64,
}

/// The CPU that did not answer within its bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lost(pub u32);

/// What the ladder asks of the machine.
pub trait Bench {
    fn cpus(&self) -> &[Cpu];
    /// MSR0000_008B on `cpu`.
    fn level(&mut self, cpu: u32) -> Result<u32, Lost>;
    /// MSRC001_0020 written on `cpu` with the address of `staged`.
    fn load(&mut self, cpu: u32, staged: Staged) -> Result<Write, Lost>;
    /// MSR0000_008B written 0 on `cpu`.
    fn write_level(&mut self, cpu: u32) -> Result<Write, Lost>;
    /// Every CPU but the caller's halted for a while; the milliseconds it was.
    fn idle(&mut self) -> Result<u64, Lost>;
    fn say(&mut self, reading: Reading);
}

/// The step a reading belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    M0,
    Guard,
    M1,
    M2,
    M3,
    M4,
    M5,
    /// The load every CPU not yet handed the intact patch makes.
    Load,
    M6,
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::M0 => "M0",
            Self::Guard => "guard",
            Self::M1 => "M1",
            Self::M2 => "M2",
            Self::M3 => "M3",
            Self::M4 => "M4",
            Self::M5 => "M5",
            Self::Load => "load",
            Self::M6 => "M6",
        })
    }
}

/// What a write came to, against the level its header names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// A #GP.
    Gp,
    /// No fault, and the level is what it was.
    Unchanged,
    /// No fault, and the level now reads the one the write named.
    Named,
    /// No fault, and the level moved to something else.
    Other,
}

impl Outcome {
    pub fn of(write: &Write, named: u32) -> Self {
        match write.fault {
            Some(_) => Self::Gp,
            None if write.after == write.before => Self::Unchanged,
            None if write.after == named => Self::Named,
            None => Self::Other,
        }
    }
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Gp => "gp",
            Self::Unchanged => "unchanged",
            Self::Named => "named",
            Self::Other => "other",
        })
    }
}

/// Why the ladder stopped or skipped a step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    /// `cpu` runs `level`, at or above the patch's.
    AtOrAbove { cpu: u32, level: u32, patch: u32 },
    /// M1 did not leave cpu0 at the patch's level.
    NotLoaded,
    /// No core but cpu0's is left that nothing has written.
    NoUnwrittenCore,
    /// No core but cpu0's is left whose level reads below the patch's.
    NoCoreBelow,
    /// cpu0's core has no other thread online.
    NoSibling,
    /// `cpu` did not answer.
    Lost { cpu: u32 },
}

impl fmt::Display for Why {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AtOrAbove { cpu, level, patch } => write!(
                f,
                "cpu{cpu} runs {level:#x}, at or above the patch's {patch:#x}, so AMD's published bytes cannot exercise a load"
            ),
            Self::NotLoaded => f.write_str("M1 did not load, and every later step reads against a load"),
            Self::NoUnwrittenCore => f.write_str("no core but cpu0's is left that nothing has written"),
            Self::NoCoreBelow => f.write_str("no core but cpu0's is left below the patch's level"),
            Self::NoSibling => f.write_str("cpu0's core has no other thread online"),
            Self::Lost { cpu } => write!(f, "cpu{cpu} did not answer"),
        }
    }
}

/// One line of the reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reading {
    Level { step: Step, cpu: Cpu, level: u32 },
    Wrote { step: Step, cpu: Cpu, what: Wrote, write: Write, outcome: Outcome },
    Idled { cpu: Cpu, millis: u64, level: u32 },
    Skipped { step: Step, why: Why },
    Stopped { step: Step, why: Why },
    Done,
}

/// What a write wrote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wrote {
    Loader(Staged),
    PatchLevel,
}

impl fmt::Display for Reading {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let cpu = |f: &mut fmt::Formatter<'_>, cpu: &Cpu| write!(f, "cpu{} apic {} core {}", cpu.id, cpu.apic, cpu.core);
        f.write_str("amducode: ")?;
        match self {
            Self::Level { step, cpu: c, level } => {
                write!(f, "{step} ")?;
                cpu(f, c)?;
                write!(f, " level {level:#x}")
            }
            Self::Wrote { step, cpu: c, what, write, outcome } => {
                write!(f, "{step} ")?;
                cpu(f, c)?;
                match what {
                    Wrote::Loader(staged) => {
                        let placement = match staged.placement {
                            Placement::Aligned => "aligned",
                            Placement::InPlace => "in-place",
                            Placement::Straddling => "straddling",
                        };
                        let bytes = match staged.bytes {
                            Bytes::Intact => "intact",
                            Bytes::BodyFlipped => "body-flipped",
                            Bytes::LevelFlipped => "level-flipped",
                        };
                        write!(f, " wrmsr 0xc0010020 {placement} {bytes} at {:#x}", write.at)?;
                    }
                    Wrote::PatchLevel => f.write_str(" wrmsr 0x8b 0")?,
                }
                let fault = Fault(write.fault);
                write!(
                    f,
                    " before {:#x} {fault} after {:#x} ticks {} ns {} outcome {outcome}",
                    write.before, write.after, write.ticks, write.nanos
                )
            }
            Self::Idled { cpu: c, millis, level } => {
                f.write_str("M6 ")?;
                cpu(f, c)?;
                write!(f, " after {millis} ms halted level {level:#x}")
            }
            Self::Skipped { step, why } => write!(f, "{step} skipped: {why}"),
            Self::Stopped { step, why } => write!(f, "{step} stop: {why}"),
            Self::Done => f.write_str("done"),
        }
    }
}

struct Fault(Option<u32>);

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(code) => write!(f, "gp {code:#x}"),
            None => f.write_str("gp none"),
        }
    }
}

/// What the ladder knows of one core.
#[derive(Clone, Copy)]
struct Core {
    id: u32,
    written: bool,
    /// The level its threads last read.
    level: u32,
}

/// Runs the ladder for the patch whose header says `level`.
pub fn run(bench: &mut impl Bench, level: u32) {
    let cpus: Vec<Cpu> = bench.cpus().to_vec();
    let mut ladder = Ladder { bench, cpus, cores: Vec::new(), handed: Vec::new(), level };
    match ladder.climb() {
        Ok(()) => ladder.bench.say(Reading::Done),
        Err(Stop(step, why)) => ladder.bench.say(Reading::Stopped { step, why }),
    }
}

struct Stop(Step, Why);

struct Ladder<'b, B> {
    bench: &'b mut B,
    cpus: Vec<Cpu>,
    cores: Vec<Core>,
    /// The CPUs the intact patch has been written on.
    handed: Vec<u32>,
    level: u32,
}

impl<B: Bench> Ladder<'_, B> {
    fn climb(&mut self) -> Result<(), Stop> {
        let cpus = self.cpus.clone();
        let Some(&first) = cpus.first() else { return Ok(()) };

        let mut above = None;
        for &cpu in &cpus {
            let level = self.read(Step::M0, cpu)?;
            if !self.cores.iter().any(|core| core.id == cpu.core) {
                self.cores.push(Core { id: cpu.core, written: false, level });
            }
            if level >= self.level && above.is_none() {
                above = Some(Why::AtOrAbove { cpu: cpu.id, level, patch: self.level });
            }
        }
        if let Some(why) = above {
            return Err(Stop(Step::M0, why));
        }

        let guard = self.bench.write_level(first.id).map_err(|Lost(cpu)| Stop(Step::Guard, Why::Lost { cpu }))?;
        self.wrote(Step::Guard, first, Wrote::PatchLevel, guard, 0);

        let m1 = self.load(Step::M1, first, INTACT)?;
        if Outcome::of(&m1, self.level) != Outcome::Named {
            return Err(Stop(Step::M1, Why::NotLoaded));
        }

        for bytes in [Bytes::BodyFlipped, Bytes::LevelFlipped] {
            match self.unwritten(first.core) {
                Some(cpu) => {
                    self.load(Step::M2, cpu, Staged { placement: Placement::Aligned, bytes })?;
                }
                None => self.bench.say(Reading::Skipped { step: Step::M2, why: Why::NoUnwrittenCore }),
            }
        }

        self.load(Step::M3, first, INTACT)?;

        match cpus.iter().find(|cpu| cpu.core == first.core && cpu.id != first.id) {
            Some(&sibling) => {
                self.read(Step::M4, sibling)?;
                self.load(Step::M4, sibling, INTACT)?;
            }
            None => self.bench.say(Reading::Skipped { step: Step::M4, why: Why::NoSibling }),
        }

        for placement in [Placement::InPlace, Placement::Straddling] {
            match self.unwritten(first.core).or_else(|| self.below(first.core)) {
                Some(cpu) => {
                    self.load(Step::M5, cpu, Staged { placement, bytes: Bytes::Intact })?;
                }
                None => self.bench.say(Reading::Skipped { step: Step::M5, why: Why::NoCoreBelow }),
            }
        }

        for &cpu in &cpus {
            if !self.handed.contains(&cpu.id) {
                self.load(Step::Load, cpu, INTACT)?;
            }
        }

        let millis = self.bench.idle().map_err(|Lost(cpu)| Stop(Step::M6, Why::Lost { cpu }))?;
        for &cpu in &cpus {
            let level = self.bench.level(cpu.id).map_err(|Lost(cpu)| Stop(Step::M6, Why::Lost { cpu }))?;
            self.bench.say(Reading::Idled { cpu, millis, level });
        }
        Ok(())
    }

    fn read(&mut self, step: Step, cpu: Cpu) -> Result<u32, Stop> {
        let level = self.bench.level(cpu.id).map_err(|Lost(cpu)| Stop(step, Why::Lost { cpu }))?;
        self.bench.say(Reading::Level { step, cpu, level });
        Ok(level)
    }

    fn load(&mut self, step: Step, cpu: Cpu, staged: Staged) -> Result<Write, Stop> {
        let write = self.bench.load(cpu.id, staged).map_err(|Lost(cpu)| Stop(step, Why::Lost { cpu }))?;
        self.wrote(step, cpu, Wrote::Loader(staged), write, staged.bytes.header_level(self.level));
        if staged.bytes == Bytes::Intact {
            self.handed.push(cpu.id);
        }
        Ok(write)
    }

    fn wrote(&mut self, step: Step, cpu: Cpu, what: Wrote, write: Write, named: u32) {
        let outcome = Outcome::of(&write, named);
        self.bench.say(Reading::Wrote { step, cpu, what, write, outcome });
        if let Some(core) = self.cores.iter_mut().find(|core| core.id == cpu.core) {
            core.written = true;
            core.level = write.after;
        }
    }

    /// The first CPU of a core nothing has written, other than `except`.
    fn unwritten(&self, except: u32) -> Option<Cpu> {
        self.first_of(except, |core| !core.written)
    }

    /// The first CPU of a core whose level reads below the patch's, other
    /// than `except`.
    fn below(&self, except: u32) -> Option<Cpu> {
        self.first_of(except, |core| core.level < self.level)
    }

    fn first_of(&self, except: u32, fit: impl Fn(&Core) -> bool) -> Option<Cpu> {
        let core = self.cores.iter().find(|core| core.id != except && fit(core))?;
        self.cpus.iter().find(|cpu| cpu.core == core.id).copied()
    }
}
