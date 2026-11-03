//! When storage and the PCI drivers come up: once the device set says the
//! root bridges' memory is held ([`toyos_abi::devset::Declaration`]), which
//! no PCI claim is handed over without.
//!
//! **Only the log and what the declaration comes through start first**
//! ([`starts_first`]): `logkeeper`, which every program's output reaches the
//! log through and which keeps no home; the `acpi` claim's holder; and where
//! none is started, a row that may mint the claim and hand it to a server of
//! its own, as a test boot's runner does.
//! Storage follows the declaration: the disk drivers, the block service, the
//! file servers, the session's home and every state directory. Every other
//! row starts after those exist, since its `HOME` is one of them, and the
//! `drives` rows with it.
//!
//! **Nothing waits where nothing can declare** ([`comes`]): a boot with no
//! `acpi` holder and no row that may hand one the claim starts every row at
//! once, storage first, and no PCI driver.
//!
//! **A wait has a failure timeout, said once**: past [`BOUND`] with nothing
//! declared, storage and the PCI drivers are given up for the boot, as when
//! the declaration is withheld. Each is said once, the storage rows' ports are
//! closed so a file request is refused rather than left waiting, and every
//! remaining row starts with no storage, as on a boot that has none.

use std::time::{Duration, Instant};

use toyos_abi::devset::{Declaration, Withheld};
use toyos_manifest::Program;

/// Whether a declaration can come on this boot: a row `started` names holds
/// the `acpi` claim, or the image carries the ACPI server (`server`) for a
/// started row that may mint the claim to hand it on. Where none can, nothing
/// waits for one: every row starts at once, and no PCI driver does.
pub fn comes(started: &[&Program], server: bool) -> bool {
    started.iter().any(|p| p.devices.iter().any(|device| device == "acpi"))
        || (server && started.iter().any(|p| p.syscap.iter().any(|right| right == "device")))
}

/// Whether `program` starts before storage, among the rows `started` names:
/// `logkeeper`; the `acpi` claim's holder; where `started` holds none, a row
/// that may mint the claim (`device`) and hand it on.
pub fn starts_first(program: &Program, started: &[&Program]) -> bool {
    let holds = |p: &Program| p.devices.iter().any(|device| device == "acpi");
    program.name == toyos_logstream::LOGKEEPER
        || holds(program)
        || (!started.iter().any(|p| holds(p)) && program.syscap.iter().any(|right| right == "device"))
}

/// How long the boot waits for the declaration before saying it has not
/// come: a failure ceiling for acpiserver's read of its namespace, which
/// takes well under a second on every machine measured.
pub const BOUND: Duration = Duration::from_secs(30);

/// What the supervisor says once the declaration is held, before how long
/// that took.
pub const HELD_SAID: &str = "supervisor: the root bridges' memory is declared ";

/// Where the boot's storage stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Storage {
    /// Not started: the declaration is awaited since `since`.
    Awaiting { since: Instant },
    /// Started on a declaration held.
    Up,
    /// Never started: no declaration will be held this boot, or none was by
    /// [`BOUND`].
    Never,
}

/// What the supervisor does on a reading of the declaration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Nothing: storage is up and the drivers follow the set as before, or it
    /// is still awaited and no line is owed.
    Nothing,
    /// Start storage, then every `drives` row: the declaration was held
    /// this long after the wait began.
    Start(Duration),
    /// Say the timeout, once, and give storage up as [`Step::Withheld`] does.
    Overdue,
    /// Say why none will be held, once, and start every remaining row with no
    /// storage.
    Withheld(Withheld),
}

impl Storage {
    pub fn awaiting(now: Instant) -> Self {
        Self::Awaiting { since: now }
    }

    /// What `declaration`, read at `now`, asks of the boot.
    pub fn read(&mut self, declaration: Declaration, now: Instant) -> Step {
        let Self::Awaiting { since } = *self else { return Step::Nothing };
        match declaration {
            Declaration::Held => {
                *self = Self::Up;
                Step::Start(now.duration_since(since))
            }
            Declaration::Withheld(why) => {
                *self = Self::Never;
                Step::Withheld(why)
            }
            Declaration::Pending if now.duration_since(since) >= BOUND => {
                *self = Self::Never;
                Step::Overdue
            }
            Declaration::Pending => Step::Nothing,
        }
    }

    /// When the loop must read again with no wake to make it: the timeout,
    /// while the declaration is awaited.
    pub fn deadline(&self) -> Option<Instant> {
        match *self {
            Self::Awaiting { since } => Some(since + BOUND),
            _ => None,
        }
    }

    /// Whether the drivers may be started.
    pub fn up(&self) -> bool {
        *self == Self::Up
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(devices: &[&str], syscap: &[&str]) -> Program {
        Program {
            devices: devices.iter().map(|d| d.to_string()).collect(),
            syscap: syscap.iter().map(|r| r.to_string()).collect(),
            ..Program::default()
        }
    }

    /// A declaration can come through a started holder, or through a started
    /// row that may mint the claim where the server is built in; else not.
    #[test]
    fn a_declaration_comes_only_through_a_holder_or_a_row_that_can_hand_the_claim() {
        let holder = row(&["acpi"], &[]);
        let runner = row(&[], &["device", "dup"]);
        let desktop = row(&[], &[]);
        assert!(comes(&[&holder, &desktop], false));
        assert!(comes(&[&runner, &desktop], true));
        assert!(!comes(&[&runner, &desktop], false));
        assert!(!comes(&[&desktop], true));
    }

    /// The log's keeper and the holder start first; a row able to mint the
    /// claim starts first only where no holder is started; every other row
    /// waits for storage.
    #[test]
    fn only_the_log_and_what_the_declaration_comes_through_start_first() {
        let holder = row(&["acpi"], &[]);
        let runner = row(&[], &["device", "dup"]);
        let keeper = Program { name: toyos_logstream::LOGKEEPER.to_string(), ..row(&[], &["logread"]) };
        let desktop = row(&[], &[]);
        let with_holder = [&holder, &runner, &keeper, &desktop];
        assert!(starts_first(&holder, &with_holder));
        assert!(!starts_first(&runner, &with_holder));
        assert!(starts_first(&keeper, &with_holder));
        assert!(!starts_first(&desktop, &with_holder));
        let without = [&runner, &keeper, &desktop];
        assert!(starts_first(&runner, &without));
        assert!(starts_first(&keeper, &without));
        assert!(!starts_first(&desktop, &without));
    }

    /// Held starts storage once, and nothing after it changes that.
    #[test]
    fn a_declaration_held_starts_storage_once() {
        let at = Instant::now();
        let mut storage = Storage::awaiting(at);
        assert!(!storage.up());
        assert_eq!(storage.read(Declaration::Pending, at), Step::Nothing);
        assert_eq!(storage.read(Declaration::Held, at + Duration::from_millis(80)), Step::Start(Duration::from_millis(80)));
        assert!(storage.up());
        for later in [Declaration::Held, Declaration::Pending, Declaration::Withheld(Withheld::Ended)] {
            assert_eq!(storage.read(later, at + BOUND * 2), Step::Nothing);
        }
        assert!(storage.up());
        assert_eq!(storage.deadline(), None);
    }

    /// The timeout is said once, at its bound and not before, and storage is
    /// given up with it: a declaration after it starts nothing.
    #[test]
    fn the_timeout_is_said_once_and_gives_storage_up() {
        let at = Instant::now();
        let mut storage = Storage::awaiting(at);
        assert_eq!(storage.deadline(), Some(at + BOUND));
        assert_eq!(storage.read(Declaration::Pending, at + BOUND.saturating_sub(Duration::from_nanos(1))), Step::Nothing);
        assert_eq!(storage.read(Declaration::Pending, at + BOUND), Step::Overdue);
        assert_eq!(storage, Storage::Never);
        assert_eq!(storage.deadline(), None);
        assert_eq!(storage.read(Declaration::Pending, at + BOUND * 3), Step::Nothing);
        assert_eq!(storage.read(Declaration::Held, at + BOUND * 3), Step::Nothing);
        assert!(!storage.up());
    }

    /// A withheld declaration is said once with its reason, and storage never
    /// starts; nor does a timeout follow it.
    #[test]
    fn a_withheld_declaration_is_said_once_and_starts_no_storage() {
        let at = Instant::now();
        for why in [Withheld::Ended, Withheld::Refused] {
            let mut storage = Storage::awaiting(at);
            assert_eq!(storage.read(Declaration::Withheld(why), at), Step::Withheld(why));
            assert_eq!(storage, Storage::Never);
            assert_eq!(storage.deadline(), None);
            for later in [Declaration::Withheld(why), Declaration::Held, Declaration::Pending] {
                assert_eq!(storage.read(later, at + BOUND * 2), Step::Nothing);
            }
            assert!(!storage.up());
        }
    }
}
