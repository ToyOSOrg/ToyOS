//! Which of `[boot] start`'s rows start before the root bridges' memory is
//! declared and which after ([`plan`]), and what each reading of the
//! declaration in the device set ([`toyos_abi::devset::Declaration`]) asks of
//! storage ([`Storage`]): no PCI claim is handed over without it, and the
//! supervisor waits for it at most [`BOUND`].

use std::time::{Duration, Instant};

use toyos_abi::devset::{Declaration, Withheld};
use toyos_manifest::Program;

/// A row holding the `acpi` claim, whose holder declares the memory no PCI
/// claim is handed over without.
pub fn declares(program: &Program) -> bool {
    program.devices.iter().any(|device| device == "acpi")
}

/// A row serving the block port.
pub fn is_block(program: &Program) -> bool {
    program.serves.iter().any(|s| s == toyos_blockring::PORT)
}

/// A `drives` row that offers its disks to the block service.
pub fn is_disk_driver(program: &Program) -> bool {
    !program.drives.is_empty() && program.receives.iter().any(|r| r == toyos_blockring::disk::PORT)
}

/// A row whose process the machine's files depend on: a disk driver, the
/// block service, or a file server. Never made a home, which would be a
/// directory on the volume it serves.
pub fn is_storage(program: &Program) -> bool {
    !program.roles.is_empty() || is_block(program) || is_disk_driver(program)
}

/// A row that may mint the `acpi` claim and hand it to a server of its own.
fn mints(program: &Program) -> bool {
    program.syscap.iter().any(|right| right == "device")
}

/// When each started row starts, by its index among the rows [`plan`] was
/// given. Index 0 is SYSTEM, running since before the manifest was read, and
/// is in none of them.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Whether a declaration can come: where none can, nothing waits for one,
    /// every row starts at once, storage first, and no PCI driver does.
    pub comes: bool,
    /// Started before the declaration: `logkeeper`, which every program's
    /// output reaches the log through and which uses no `HOME`; the `acpi`
    /// claim's holder; and where none is started, a row that may mint the
    /// claim and hand it on, as a test boot's runner does. Empty where none
    /// can come.
    pub first: Vec<usize>,
    /// The storage rows, started once the declaration is held.
    pub storage: Vec<usize>,
    /// Every other row, started once storage and the homes are.
    pub rest: Vec<usize>,
}

/// The plan for `rows`, `[boot] start`'s in the order they start, SYSTEM
/// first. `holder` says whether the manifest declares a row holding the
/// `acpi` claim, started or not, which a started row that mints the claim
/// may hand it to.
pub fn plan(rows: &[&Program], holder: bool) -> Plan {
    let started = || rows.iter().skip(1);
    let held = started().any(|p| declares(p));
    let comes = held || (holder && started().any(|p| mints(p)));
    let first_here = |p: &Program| comes && (p.name == toyos_logstream::LOGKEEPER || declares(p) || (!held && mints(p)));
    let mut plan = Plan { comes, first: Vec::new(), storage: Vec::new(), rest: Vec::new() };
    for (at, program) in rows.iter().enumerate().skip(1) {
        match () {
            () if first_here(program) => plan.first.push(at),
            () if is_storage(program) => plan.storage.push(at),
            () => plan.rest.push(at),
        }
    }
    plan
}

/// How long the boot waits for the declaration before saying it has not
/// come: a failure ceiling for acpiserver's read of its namespace, which
/// takes well under a second on every machine measured.
pub const BOUND: Duration = Duration::from_secs(30);

/// Where the boot's storage stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Storage {
    /// Not started: the declaration is awaited since `since`.
    Awaiting { since: Instant },
    /// Being started: the storage rows, the homes and every other row, in
    /// that order. A service that ends meanwhile is started again, and so is a
    /// disk driver, which the homes are made through; no other `drives` row
    /// starts until [`Storage::finish`], so none starts before its home
    /// exists ([`Storage::may_start`]). `drivers` as in [`Storage::Up`].
    Starting { drivers: bool },
    /// Started: `drivers` where a declaration is held, so the PCI drivers may
    /// claim; not where nothing can declare.
    Up { drivers: bool },
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
    /// Awaiting the declaration from `now`, or starting with no driver where
    /// none can come ([`Plan::comes`]).
    pub fn new(comes: bool, now: Instant) -> Self {
        match comes {
            true => Self::Awaiting { since: now },
            false => Self::Starting { drivers: false },
        }
    }

    /// Storage, the homes and every other row are started: the `drives` rows
    /// may follow.
    pub fn finish(&mut self) {
        let Self::Starting { drivers } = *self else { panic!("supervisor: storage finished from {self:?}") };
        *self = Self::Up { drivers };
    }

    /// What `declaration`, read at `now`, asks of the boot. A declaration
    /// held is read first, so one read at or past [`BOUND`] still starts
    /// storage.
    pub fn read(&mut self, declaration: Declaration, now: Instant) -> Step {
        let Self::Awaiting { since } = *self else { return Step::Nothing };
        match declaration {
            Declaration::Held => {
                *self = Self::Starting { drivers: true };
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

    /// Whether storage is started, so a home or state directory can be made.
    pub fn started(&self) -> bool {
        matches!(self, Self::Starting { .. } | Self::Up { .. })
    }

    /// Whether a declaration is held, so storage's own disk drivers may claim
    /// at the start of its window.
    pub fn declared(&self) -> bool {
        matches!(self, Self::Starting { drivers: true } | Self::Up { drivers: true })
    }

    /// Whether the PCI drivers may be started: not while storage and the homes
    /// are being started.
    pub fn drivers(&self) -> bool {
        *self == Self::Up { drivers: true }
    }

    /// Whether `program`'s `drives` row may start a process now: every one
    /// once storage is up on a declaration; inside storage's window a disk
    /// driver alone, since the homes the window makes go through it.
    pub fn may_start(&self, program: &Program) -> bool {
        self.drivers() || (self.declared() && is_disk_driver(program))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, devices: &[&str], syscap: &[&str]) -> Program {
        Program {
            name: name.to_string(),
            devices: devices.iter().map(|d| d.to_string()).collect(),
            syscap: syscap.iter().map(|r| r.to_string()).collect(),
            ..Program::default()
        }
    }

    fn system() -> Program {
        Program { roles: vec!["system".to_string()], ..row("fileserver", &[], &[]) }
    }

    fn storage(name: &str) -> Program {
        Program { roles: vec!["data".to_string()], ..row(name, &[], &[]) }
    }

    fn keeper() -> Program {
        row(toyos_logstream::LOGKEEPER, &[], &["logread"])
    }

    /// SYSTEM is in no list, whatever the boot: it is never started again by
    /// the held path and its ports are never among those a withheld or
    /// overdue boot closes, which are `storage`'s alone.
    #[test]
    fn system_is_in_no_list() {
        let (system, holder, keeper, data, desktop) = (system(), row("acpiserver", &["acpi"], &[]), keeper(), storage("fileserver"), row("compositor", &[], &[]));
        for rows in [vec![&system, &holder, &keeper, &data, &desktop], vec![&system, &keeper, &data, &desktop]] {
            for holder in [true, false] {
                let plan = plan(&rows, holder);
                assert!(!plan.first.contains(&0) && !plan.storage.contains(&0) && !plan.rest.contains(&0), "{plan:?}");
                let mut all: Vec<usize> = plan.first.iter().chain(&plan.storage).chain(&plan.rest).copied().collect();
                all.sort();
                assert_eq!(all, (1..rows.len()).collect::<Vec<_>>(), "every other row once: {plan:?}");
            }
        }
    }

    /// The log's keeper and the holder start first, storage on the
    /// declaration and every other row after it; a row that may mint the claim
    /// starts first only where no holder is started and one is declared.
    #[test]
    fn the_log_and_what_the_declaration_comes_through_start_first() {
        let (system, holder, keeper, data, desktop) = (system(), row("acpiserver", &["acpi"], &[]), keeper(), storage("fileserver"), row("compositor", &[], &[]));
        let runner = row("test-runner", &[], &["device", "dup"]);
        let with_holder = plan(&[&system, &keeper, &holder, &runner, &data, &desktop], true);
        assert_eq!(with_holder, Plan { comes: true, first: vec![1, 2], storage: vec![4], rest: vec![3, 5] });
        let by_a_job = plan(&[&system, &keeper, &runner, &data, &desktop], true);
        assert_eq!(by_a_job, Plan { comes: true, first: vec![1, 2], storage: vec![3], rest: vec![4] });
    }

    /// Where nothing can declare, no row is first and storage leads the rest.
    #[test]
    fn nothing_waits_where_nothing_can_declare() {
        let (system, keeper, data, desktop) = (system(), keeper(), storage("fileserver"), row("compositor", &[], &[]));
        let runner = row("test-runner", &[], &["device", "dup"]);
        for (rows, holder) in [(vec![&system, &keeper, &data, &desktop], true), (vec![&system, &keeper, &runner, &data], false)] {
            let plan = plan(&rows, holder);
            assert!(!plan.comes && plan.first.is_empty(), "{plan:?}");
        }
        let mut storage = Storage::new(false, Instant::now());
        assert_eq!(storage, Storage::Starting { drivers: false });
        storage.finish();
        assert_eq!(storage, Storage::Up { drivers: false });
        assert!(storage.started() && !storage.drivers());
    }

    /// Held starts storage once, and nothing after it changes that.
    #[test]
    fn a_declaration_held_starts_storage_once() {
        let at = Instant::now();
        let mut storage = Storage::new(true, at);
        assert!(!storage.started() && !storage.drivers());
        assert_eq!(storage.read(Declaration::Pending, at), Step::Nothing);
        assert_eq!(storage.read(Declaration::Held, at + Duration::from_millis(80)), Step::Start(Duration::from_millis(80)));
        assert!(storage.started() && !storage.drivers());
        storage.finish();
        assert!(storage.started() && storage.drivers());
        for later in [Declaration::Held, Declaration::Pending, Declaration::Withheld(Withheld::Ended)] {
            assert_eq!(storage.read(later, at + BOUND * 2), Step::Nothing);
        }
        assert_eq!(storage.deadline(), None);
    }

    /// A row's end inside storage's window: the reading of the set it causes
    /// starts a disk driver again, which the homes are made through, and no
    /// other `drives` row, which waits for [`Storage::finish`], after every
    /// home. On a boot nothing can declare on, neither starts.
    #[test]
    fn only_a_disk_driver_starts_inside_storages_window() {
        let disk = Program {
            drives: vec!["pci:01:08:02".to_string()],
            receives: vec![toyos_blockring::disk::PORT.to_string()],
            ..row("nvme", &[], &[])
        };
        let console = Program { drives: vec!["pci:07:80:00/1af4:1043".to_string()], ..row("virtio-console", &[], &[]) };
        let at = Instant::now();
        let mut storage = Storage::new(true, at);
        assert!(!storage.may_start(&disk) && !storage.may_start(&console), "nothing before the declaration");
        assert_eq!(storage.read(Declaration::Held, at), Step::Start(Duration::ZERO));
        // The end's reading of the set, inside the window.
        assert_eq!(storage.read(Declaration::Held, at + Duration::from_millis(5)), Step::Nothing);
        assert!(storage.started(), "homes may be made in the window");
        assert!(storage.may_start(&disk), "a disk driver that ends inside the window is the homes' way to disk");
        assert!(!storage.may_start(&console), "a drives row inside the window starts before its home");
        storage.finish();
        assert!(storage.may_start(&disk) && storage.may_start(&console));
        let mut none = Storage::new(false, at);
        assert!(!none.may_start(&disk) && !none.may_start(&console));
        none.finish();
        assert!(!none.may_start(&disk) && !none.may_start(&console));
    }

    /// A declaration read at its bound, or past it while the boot still
    /// awaits it, starts storage: held is read before the bound.
    #[test]
    fn a_declaration_held_at_the_bound_still_starts_storage() {
        let at = Instant::now();
        for late in [at + BOUND, at + BOUND * 2] {
            let mut storage = Storage::new(true, at);
            assert_eq!(storage.read(Declaration::Held, late), Step::Start(late - at));
        }
    }

    /// The timeout is said once, at its bound and not before, and storage is
    /// given up with it: a declaration after it starts nothing.
    #[test]
    fn the_timeout_is_said_once_and_gives_storage_up() {
        let at = Instant::now();
        let mut storage = Storage::new(true, at);
        assert_eq!(storage.deadline(), Some(at + BOUND));
        assert_eq!(storage.read(Declaration::Pending, at + BOUND.saturating_sub(Duration::from_nanos(1))), Step::Nothing);
        assert_eq!(storage.read(Declaration::Pending, at + BOUND), Step::Overdue);
        assert_eq!(storage, Storage::Never);
        assert_eq!(storage.deadline(), None);
        assert_eq!(storage.read(Declaration::Pending, at + BOUND * 3), Step::Nothing);
        assert_eq!(storage.read(Declaration::Held, at + BOUND * 3), Step::Nothing);
        assert!(!storage.started());
    }

    /// A withheld declaration is said once with its reason, and storage never
    /// starts; nor does a timeout follow it.
    #[test]
    fn a_withheld_declaration_is_said_once_and_starts_no_storage() {
        let at = Instant::now();
        for why in [Withheld::Ended, Withheld::Refused] {
            let mut storage = Storage::new(true, at);
            assert_eq!(storage.read(Declaration::Withheld(why), at), Step::Withheld(why));
            assert_eq!(storage, Storage::Never);
            assert_eq!(storage.deadline(), None);
            for later in [Declaration::Withheld(why), Declaration::Held, Declaration::Pending] {
                assert_eq!(storage.read(later, at + BOUND * 2), Step::Nothing);
            }
            assert!(!storage.started());
        }
    }
}
