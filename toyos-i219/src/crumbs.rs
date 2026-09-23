//! Crumbs: one durable line before each step, so a machine that ends without a
//! record has still said which step it was in.
//!
//! **The order is the whole instrument, and [`before`] is the one place that
//! decides it**: the crumb is made durable and only then is the step taken. A
//! crumb written after its step names the step that *returned*, and on a
//! machine that ends inside a step that is the wrong one.
//!
//! **What the last crumb says is bounded on both sides.** Crumb *n* on the
//! device means step *n − 1* returned. It does not mean step *n* was reached:
//! the machine may have ended while crumb *n + 1* was still being written,
//! which is after step *n* returned. [`Line::synced`] is what narrows that —
//! each line carries when the line before it became durable, so a reader has
//! the width of every step and of every write but the last.
//!
//! **A crumbed part is the same part.** [`Crumbed`] forwards every access it is
//! given, in the order it is given them, and decides nothing: the driver above
//! it cannot tell it from the [`Registers`] underneath.
//!
//! **[`Step::Saw`] is the one crumb left after its access, and it is the only
//! one that carries what a read answered.** A read's own [`Step::Read`] says
//! which register is about to be reached and cannot say what came back, so a
//! reading that has to survive the machine is a second line behind the first:
//! the pair brackets the access, and a trail that carries the `read` and not
//! the `saw` is a machine that ended inside it.

use crate::{regs, Registers};

/// Where crumbs go.
pub trait Trail {
    /// Make `step` durable. **Returns only once it is**, because what the
    /// caller does next may be the last thing the machine does.
    fn crumb(&self, step: Step);
}

impl<T: Trail> Trail for &T {
    fn crumb(&self, step: Step) {
        (**self).crumb(step);
    }
}

/// The trail of a boot that leaves none.
pub struct Silent;

impl Trail for Silent {
    fn crumb(&self, _: Step) {}
}

/// Leave `step`'s crumb, and only then take the step.
pub fn before<T: Trail, A>(trail: &T, step: Step, take: impl FnOnce() -> A) -> A {
    trail.crumb(step);
    take()
}

/// One thing a bring-up does that reaches the kernel's claim or the part.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    /// The process has started and holds nothing yet.
    Start,
    /// The claim its parent moved into it is in its hands.
    ClaimHeld,
    /// The claim is asked what the function's BARs are.
    Describe,
    /// The register BAR is mapped.
    MapBar,
    /// The DMA grant is asked for, which is what gives the function memory it
    /// can reach.
    DmaAlloc,
    /// One register read, or the first of a run of them.
    Read { reg: usize },
    /// One register write, or the first of a run of them.
    Write { reg: usize, value: u32 },
    /// What one register read answered, left *after* that read returned — the
    /// only crumb of the two orders, and never one of a run.
    Saw { reg: usize, value: u32 },
    /// The bring-up returned.
    Opened,
    /// The process ends with this code, which is also what gives the claim up.
    Exit { code: i32 },
}

/// A 32-bit word as a crumb spells it.
fn word(text: &str) -> Option<u32> {
    u32::from_str_radix(text.strip_prefix("0x")?, 16).ok()
}

/// The registers a bring-up reaches, by the names the datasheet gives them.
const NAMES: [(usize, &str); 35] = [
    (regs::CTRL, "CTRL"),
    (regs::CTRL_EXT, "CTRL_EXT"),
    (regs::PHY_CTRL, "PHY_CTRL"),
    (regs::FWSM, "FWSM"),
    (regs::STATUS, "STATUS"),
    (regs::MDIC, "MDIC"),
    (regs::ICR, "ICR"),
    (regs::ITR, "ITR"),
    (regs::ICS, "ICS"),
    (regs::IMS, "IMS"),
    (regs::IMC, "IMC"),
    (regs::EIAC, "EIAC"),
    (regs::IVAR, "IVAR"),
    (regs::RCTL, "RCTL"),
    (regs::RDBAL, "RDBAL"),
    (regs::RDBAH, "RDBAH"),
    (regs::RDLEN, "RDLEN"),
    (regs::RDH, "RDH"),
    (regs::RDT, "RDT"),
    (regs::RDTR, "RDTR"),
    (regs::RADV, "RADV"),
    (regs::RAL0, "RAL0"),
    (regs::RAH0, "RAH0"),
    (regs::TCTL, "TCTL"),
    (regs::TIPG, "TIPG"),
    (regs::TDBAL, "TDBAL"),
    (regs::TDBAH, "TDBAH"),
    (regs::TDLEN, "TDLEN"),
    (regs::TDH, "TDH"),
    (regs::TDT, "TDT"),
    (regs::TIDV, "TIDV"),
    (regs::TXDCTL, "TXDCTL"),
    (regs::TADV, "TADV"),
    (regs::EXTCNF_CTRL, "EXTCNF_CTRL"),
    (regs::MTA, MTA_NAME),
];

/// Every one of §10.2.5.21's 128 entries, which a bring-up writes as one run.
const MTA_NAME: &str = "MTA";

/// A register offset as a crumb spells it: the datasheet's name, or the offset
/// for a register that has none here.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Register(usize);

impl Register {
    fn name(self) -> Option<&'static str> {
        if (regs::MTA..regs::MTA + regs::MTA_DWORDS * 4).contains(&self.0) {
            return Some(MTA_NAME);
        }
        NAMES.iter().find(|(reg, _)| *reg == self.0).map(|(_, name)| *name)
    }

    fn parse(word: &str) -> Option<usize> {
        if let Some((reg, _)) = NAMES.iter().find(|(_, name)| *name == word) {
            return Some(*reg);
        }
        usize::from_str_radix(word.strip_prefix("0x")?, 16).ok()
    }
}

impl core::fmt::Display for Register {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.name() {
            Some(name) => f.write_str(name),
            None => write!(f, "{:#07x}", self.0),
        }
    }
}

impl Step {
    /// Whether this access belongs to the run `last` began: the same kind of
    /// access to the same named register, which is what a poll and a table
    /// fill both are.
    ///
    /// **Never on `EXTCNF_CTRL`**: every access to §4.5.2's arbitration is a
    /// move in a handshake another agent is party to, so each is a step of its
    /// own. **Never on [`Step::Saw`]** either: a reading is not an access, and
    /// two of them are two answers the part gave and not one step repeated.
    fn continues(self, last: Step) -> bool {
        let (reg, was) = match (self, last) {
            (Self::Read { reg }, Self::Read { reg: was }) => (reg, was),
            (Self::Write { reg, .. }, Self::Write { reg: was, .. }) => (reg, was),
            _ => return false,
        };
        let same = match Register(reg).name() {
            Some(name) => Register(was).name() == Some(name),
            None => reg == was,
        };
        same && reg != regs::EXTCNF_CTRL
    }

    /// The step with a written or read value left off: what two trails of one
    /// bring-up agree on whatever the part's registers held.
    pub fn named(&self) -> Named {
        Named(*self)
    }

    fn parse(text: &str) -> Option<Self> {
        let mut words = text.split(' ');
        let step = match (words.next()?, words.next(), words.next()) {
            ("start", None, None) => Self::Start,
            ("claim-held", None, None) => Self::ClaimHeld,
            ("describe", None, None) => Self::Describe,
            ("map-bar", None, None) => Self::MapBar,
            ("dma-alloc", None, None) => Self::DmaAlloc,
            ("opened", None, None) => Self::Opened,
            ("read", Some(reg), None) => Self::Read { reg: Register::parse(reg)? },
            ("write", Some(reg), Some(value)) => {
                Self::Write { reg: Register::parse(reg)?, value: word(value)? }
            }
            ("saw", Some(reg), Some(value)) => {
                Self::Saw { reg: Register::parse(reg)?, value: word(value)? }
            }
            ("exit", Some(code), None) => Self::Exit { code: code.parse().ok()? },
            _ => return None,
        };
        words.next().is_none().then_some(step)
    }
}

impl core::fmt::Display for Step {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Start => f.write_str("start"),
            Self::ClaimHeld => f.write_str("claim-held"),
            Self::Describe => f.write_str("describe"),
            Self::MapBar => f.write_str("map-bar"),
            Self::DmaAlloc => f.write_str("dma-alloc"),
            Self::Read { reg } => write!(f, "read {}", Register(*reg)),
            Self::Write { reg, value } => write!(f, "write {} {value:#010x}", Register(*reg)),
            Self::Saw { reg, value } => write!(f, "saw {} {value:#010x}", Register(*reg)),
            Self::Opened => f.write_str("opened"),
            Self::Exit { code } => write!(f, "exit {code}"),
        }
    }
}

/// [`Step::named`]'s spelling.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Named(Step);

impl core::fmt::Display for Named {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self.0 {
            Step::Write { reg, .. } => write!(f, "write {}", Register(reg)),
            Step::Saw { reg, .. } => write!(f, "saw {}", Register(reg)),
            step => write!(f, "{step}"),
        }
    }
}

/// What [`crate::I219::open`]'s trail owes between [`Step::DmaAlloc`] and
/// [`Step::Opened`], each as [`Step::named`] spells it and with a run as the one
/// crumb [`Runs`] makes of it. Written from §4.6's order: §4.6.1's masked reset
/// behind §3.1.3.10's quiesce, the station address, §4.6.5's table, the link,
/// the vector allocation and moderation, §4.6.5.1's and §4.6.6's rings, the
/// transmitter, the receiver, the mask, the link's first reading.
///
/// **The PHY's accesses are not in it**: how many there are is the
/// arbitration's answer and the PHY's, so a reader takes `EXTCNF_CTRL` and
/// `MDIC` out of a trail before holding it to this.
pub const BRING_UP: [&str; 42] = [
    "read STATUS",
    "write IMC",
    "read ICR",
    "read CTRL",
    "write CTRL",
    "read STATUS",
    "read CTRL",
    "write CTRL",
    "read CTRL",
    "write IMC",
    "read ICR",
    "read RAL0",
    "read RAH0",
    "write MTA",
    "read CTRL",
    "write CTRL",
    "write EIAC",
    "write IVAR",
    "read IVAR",
    "write ITR",
    "write RDTR",
    "write RADV",
    "write TIDV",
    "write TADV",
    "write RDBAL",
    "write RDBAH",
    "write RDLEN",
    "write RDH",
    "write RDT",
    "write TDBAL",
    "write TDBAH",
    "write TDLEN",
    "write TDH",
    "write TDT",
    "write TXDCTL",
    "write TIPG",
    "write TCTL",
    "read TCTL",
    "write RCTL",
    "read RCTL",
    "write IMS",
    "read STATUS",
];

/// Whether a named step is one of the PHY's, which [`BRING_UP`] leaves out.
///
/// **A reading is always the PHY's.** [`Step::Saw`] is left by the bring-up's
/// power step and by the bench instrument and by nothing else, so a line that
/// starts with one is the PHY's whatever register it names.
///
/// **`CTRL` is not here and cannot be**: §8.2.1's `PHYPDN` is a field of it,
/// so the power step reaches the same register the bring-up reaches either
/// side of it. A reader comparing a trail with [`BRING_UP`] has to let a
/// `CTRL` access be extra — [`is_shared_with_the_phys`] is that rule.
pub fn is_the_phys(named: &str) -> bool {
    named.starts_with("saw ")
        || named.ends_with(" EXTCNF_CTRL")
        || named.ends_with(" MDIC")
        || named.ends_with(" CTRL_EXT")
        || named.ends_with(" PHY_CTRL")
        || named.ends_with(" FWSM")
}

/// Whether a named step is one [`BRING_UP`] names *and* the PHY's power step
/// reaches too, so a trail may carry more of them than the table does.
pub fn is_shared_with_the_phys(named: &str) -> bool {
    named.ends_with(" CTRL")
}

/// A trail that takes the first access of a run and lets the rest of it by.
///
/// **A poll is one step.** A driver spins on a status bit for as long as its
/// deadline lets it, and a durable line per read would turn a wait the part
/// bounds into one the log device does.
pub struct Runs<T> {
    trail: T,
    last: core::cell::Cell<Option<Step>>,
}

impl<T: Trail> Runs<T> {
    pub fn over(trail: T) -> Self {
        Self { trail, last: core::cell::Cell::new(None) }
    }
}

impl<T: Trail> Trail for Runs<T> {
    fn crumb(&self, step: Step) {
        if self.last.get().is_some_and(|last| step.continues(last)) {
            return;
        }
        self.last.set(Some(step));
        self.trail.crumb(step);
    }
}

/// A register window that leaves a crumb before each access it forwards.
pub struct Crumbed<R, T> {
    regs: R,
    trail: T,
}

impl<R: Registers, T: Trail> Crumbed<R, T> {
    pub fn over(regs: R, trail: T) -> Self {
        Self { regs, trail }
    }
}

impl<R: Registers, T: Trail> Registers for Crumbed<R, T> {
    fn bytes(&self) -> usize {
        self.regs.bytes()
    }

    fn read(&self, reg: usize) -> u32 {
        before(&self.trail, Step::Read { reg }, || self.regs.read(reg))
    }

    fn write(&self, reg: usize, value: u32) {
        before(&self.trail, Step::Write { reg, value }, || self.regs.write(reg, value));
    }
}

/// One line of the crumb file, which both ends spell through this type.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Line {
    /// Counted from zero, so a missing line is a gap and not a guess.
    pub seq: u32,
    /// The monotonic clock when this line was handed to the device.
    pub at: u64,
    /// The same clock when the line before this one came back durable, and zero
    /// on the first. `at - synced` is how long the step before this one took;
    /// the next line's `synced - at` is how long this line took to write.
    pub synced: u64,
    pub step: Step,
}

impl Line {
    fn parse(text: &str) -> Option<Self> {
        let mut words = text.splitn(4, ' ');
        Some(Self {
            seq: words.next()?.parse().ok()?,
            at: words.next()?.parse().ok()?,
            synced: words.next()?.parse().ok()?,
            step: Step::parse(words.next()?)?,
        })
    }
}

impl core::fmt::Display for Line {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:03} {} {} {}", self.seq, self.at, self.synced, self.step)
    }
}

/// Why a crumb file is not a trail.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Broken<'a> {
    /// A whole line — one the file carries a newline after — that is no crumb.
    Unreadable { line: &'a str },
    /// A line out of sequence, so a crumb is missing from the middle.
    Gap { wanted: u32, line: Line },
}

impl core::fmt::Display for Broken<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unreadable { line } => write!(f, "{line:?} ends in a newline and is no crumb"),
            Self::Gap { wanted, line } => {
                write!(f, "crumb {wanted} is missing: the line where it belongs is `{line}`")
            }
        }
    }
}

/// Every crumb of a file, in order.
///
/// **A last line with no newline after it is the write the machine ended in**,
/// and is left out rather than refused: the line before it is the last crumb
/// that was durable, which is the question.
pub fn lines(text: &str) -> impl Iterator<Item = Result<Line, Broken<'_>>> {
    let whole = text.rfind('\n').map_or("", |end| &text[..=end]);
    whole.lines().enumerate().map(|(wanted, text)| {
        let line = Line::parse(text).ok_or(Broken::Unreadable { line: text })?;
        if line.seq as usize != wanted {
            return Err(Broken::Gap { wanted: wanted as u32, line });
        }
        Ok(line)
    })
}

/// Where a trail stops, which is what a machine that left no other record said
/// about its own end.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ending {
    /// No crumb at all: the machine ended before the first line was durable.
    Nothing,
    /// The trail's last line is some step's, and nothing says it returned.
    In(Line),
    /// The trail ends at [`Step::Exit`]: every step of the process returned.
    Complete { last: Line, code: i32 },
}

impl Ending {
    pub fn of(text: &str) -> Result<Self, Broken<'_>> {
        let mut last = None;
        for line in lines(text) {
            last = Some(line?);
        }
        Ok(match last {
            None => Self::Nothing,
            Some(last) => match last.step {
                Step::Exit { code } => Self::Complete { last, code },
                _ => Self::In(last),
            },
        })
    }
}

impl core::fmt::Display for Ending {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Nothing => write!(
                f,
                "no crumb is on the device, so the machine ended before this process's first \
                 line was durable — before it held the claim, and so before it reached the part"
            ),
            Self::In(line) => write!(
                f,
                "the last crumb on the device is {} `{}` at {} ns: the machine ended inside that \
                 step, or after it returned and before the next crumb was durable",
                line.seq, line.step, line.at
            ),
            Self::Complete { last, code } => write!(
                f,
                "all {} crumbs are on the device and the last is `exit {code}` at {} ns: every \
                 step of this process returned, so whatever ended the machine came after it — \
                 the kernel giving the claim up, its reset of the function, or later still",
                last.seq + 1,
                last.at
            ),
        }
    }
}
