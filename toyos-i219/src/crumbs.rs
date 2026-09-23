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
//! **[`Witnessed`] closes the last crumb's other side.** A trail of `before`
//! lines alone cannot separate "the machine ended inside step *n*" from "step
//! *n* returned and crumb *n + 1* was still being written"; a line on each side
//! of an access can, and the `after` line carries what a read answered. It
//! costs two durable writes per access, so it is for an arm whose whole purpose
//! is the last line and not for a bring-up.

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

/// Leave a durable line on each side of `deed`.
///
/// **The first line is durable before the deed is taken** and not after it,
/// because the deed may be the last thing this machine does: a line written
/// afterwards would never reach the device, and the trail would name the deed
/// before this one. The second line is what says this deed returned.
pub fn around<T: Trail, A>(trail: &T, deed: Deed, take: impl FnOnce() -> A) -> A {
    trail.crumb(Step::Before { deed });
    let took = take();
    trail.crumb(Step::After { deed });
    took
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
    /// The bring-up returned.
    Opened,
    /// The process ends with this code, which is also what gives the claim up.
    Exit { code: i32 },
    /// The deed named is about to be taken. **A trail that ends here says the
    /// deed itself is what the machine did not come back from.**
    Before { deed: Deed },
    /// The deed named returned, with what it carried. **A trail that ends here
    /// says the death is later than this deed.**
    After { deed: Deed },
}

/// One thing a [`Witnessed`] trail names on both sides.
///
/// Separate from [`Step`]'s own `Read` and `Write` because a witnessed read
/// carries what it answered, which the line before it cannot have.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Deed {
    /// One register read. `value` is what it answered, so it is `None` on the
    /// line before the access and `Some` on the line after it.
    Read { reg: usize, value: Option<u32> },
    /// One register write, with the word written — the same on both lines.
    Write { reg: usize, value: u32 },
    /// The claim kept across a wait that reaches no register. Giving the claim
    /// up is what makes the kernel reset the function, so a trail that ends
    /// inside this one ends with the function still this process's.
    Hold,
}

impl Deed {
    /// Whether these two lines are the two sides of one access: the same kind
    /// of access to the same register carrying the same word, a read's answer
    /// apart.
    pub fn is_the_same_deed(self, other: Self) -> bool {
        match (self, other) {
            (Self::Read { reg, .. }, Self::Read { reg: was, .. }) => reg == was,
            (Self::Write { reg, value }, Self::Write { reg: was, value: wrote }) => {
                reg == was && value == wrote
            }
            (Self::Hold, Self::Hold) => true,
            _ => false,
        }
    }

    fn parse(text: &str) -> Option<Self> {
        let mut words = text.split(' ');
        let deed = match (words.next()?, words.next(), words.next()) {
            ("hold", None, None) => Self::Hold,
            ("read", Some(reg), None) => Self::Read { reg: Register::parse(reg)?, value: None },
            ("read", Some(reg), Some(value)) => {
                Self::Read { reg: Register::parse(reg)?, value: Some(word(value)?) }
            }
            ("write", Some(reg), Some(value)) => {
                Self::Write { reg: Register::parse(reg)?, value: word(value)? }
            }
            _ => return None,
        };
        words.next().is_none().then_some(deed)
    }
}

impl core::fmt::Display for Deed {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Read { reg, value: None } => write!(f, "read {}", Register(*reg)),
            Self::Read { reg, value: Some(value) } => {
                write!(f, "read {} {value:#010x}", Register(*reg))
            }
            Self::Write { reg, value } => write!(f, "write {} {value:#010x}", Register(*reg)),
            Self::Hold => f.write_str("hold"),
        }
    }
}

/// A 32-bit word as a crumb spells it.
fn word(text: &str) -> Option<u32> {
    u32::from_str_radix(text.strip_prefix("0x")?, 16).ok()
}

/// The registers a bring-up reaches, by the names the datasheet gives them.
const NAMES: [(usize, &str); 32] = [
    (regs::CTRL, "CTRL"),
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
    /// own. **Never a [`Witnessed`] line either** — a fold there would leave
    /// one line standing for two accesses, which is the one thing that trail
    /// exists not to do.
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

    /// The step with a write's value left off: what two trails of one bring-up
    /// agree on whatever the part's registers held.
    pub fn named(&self) -> Named {
        Named(*self)
    }

    fn parse(text: &str) -> Option<Self> {
        if let Some(deed) = text.strip_prefix("before ") {
            return Some(Self::Before { deed: Deed::parse(deed)? });
        }
        if let Some(deed) = text.strip_prefix("after ") {
            return Some(Self::After { deed: Deed::parse(deed)? });
        }
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
            Self::Opened => f.write_str("opened"),
            Self::Exit { code } => write!(f, "exit {code}"),
            Self::Before { deed } => write!(f, "before {deed}"),
            Self::After { deed } => write!(f, "after {deed}"),
        }
    }
}

/// [`Step::named`]'s spelling.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Named(Step);

impl core::fmt::Display for Named {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let side = |deed: Deed, side: &str, f: &mut core::fmt::Formatter<'_>| match deed {
            Deed::Read { reg, .. } => write!(f, "{side} read {}", Register(reg)),
            Deed::Write { reg, .. } => write!(f, "{side} write {}", Register(reg)),
            Deed::Hold => write!(f, "{side} hold"),
        };
        match self.0 {
            Step::Write { reg, .. } => write!(f, "write {}", Register(reg)),
            Step::Before { deed } => side(deed, "before", f),
            Step::After { deed } => side(deed, "after", f),
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
pub fn is_the_phys(named: &str) -> bool {
    named.ends_with(" EXTCNF_CTRL") || named.ends_with(" MDIC")
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

/// A register window that leaves a durable crumb on **both** sides of every
/// access it forwards, the `after` line carrying what a read answered.
///
/// **What it buys over [`Crumbed`]** is the one question a machine that dies
/// mid-access leaves: a trail ending `before write <reg> <value>` names that
/// write, and one ending `after write <reg> <value>` says the write returned
/// and the death is later. **What it costs** is two durable writes per access,
/// which stretches every wait this driver takes on a clock — a poll bounded by
/// a deadline then fits fewer samples in the same bound. It moves no access, no
/// value and no order: like [`Crumbed`], it forwards exactly what it is given.
pub struct Witnessed<R, T> {
    regs: R,
    trail: T,
}

impl<R: Registers, T: Trail> Witnessed<R, T> {
    pub fn over(regs: R, trail: T) -> Self {
        Self { regs, trail }
    }
}

impl<R: Registers, T: Trail> Registers for Witnessed<R, T> {
    fn bytes(&self) -> usize {
        self.regs.bytes()
    }

    fn read(&self, reg: usize) -> u32 {
        // The first line is durable before the access because the access may be
        // the last thing this machine does; the second carries the word it
        // answered, which is what the register held at that moment.
        self.trail.crumb(Step::Before { deed: Deed::Read { reg, value: None } });
        let value = self.regs.read(reg);
        self.trail.crumb(Step::After { deed: Deed::Read { reg, value: Some(value) } });
        value
    }

    fn write(&self, reg: usize, value: u32) {
        around(&self.trail, Deed::Write { reg, value }, || self.regs.write(reg, value));
    }
}

/// Why a trail's witnessed lines are not pairs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unpaired {
    /// A `before` line the next line is not the `after` of.
    Interrupted { before: Line, next: Line },
    /// An `after` line with no `before` of its own in front of it.
    Stray { after: Line },
}

impl core::fmt::Display for Unpaired {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Interrupted { before, next } => write!(
                f,
                "crumb {} is `{}` and the line after it is `{}`, which is not that deed's other \
                 side",
                before.seq, before.step, next.step
            ),
            Self::Stray { after } => {
                write!(f, "crumb {} is `{}` with no `before` line of its own", after.seq, after.step)
            }
        }
    }
}

/// Every witnessed deed of a trail as the pair of lines around it: how many
/// pairs closed, and the deed the trail stops inside if it stops inside one.
///
/// **The unclosed last deed is the reading a dead machine leaves**, so it is
/// handed back rather than refused; a `before` followed by anything *else* is a
/// refusal, because a trail that interleaves two deeds cannot say which one the
/// last line is about.
pub fn witnessed(lines: &[Line]) -> Result<(usize, Option<Deed>), Unpaired> {
    let mut pairs = 0;
    let mut open = None;
    let mut at = 0;
    while at < lines.len() {
        let line = lines[at];
        match line.step {
            Step::Before { deed } => {
                let Some(next) = lines.get(at + 1) else {
                    open = Some(deed);
                    break;
                };
                match next.step {
                    Step::After { deed: back } if deed.is_the_same_deed(back) => at += 1,
                    _ => return Err(Unpaired::Interrupted { before: line, next: *next }),
                }
                pairs += 1;
            }
            Step::After { .. } => return Err(Unpaired::Stray { after: line }),
            _ => {}
        }
        at += 1;
    }
    Ok((pairs, open))
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
