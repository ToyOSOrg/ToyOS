//! What a process names a kernel object with, and what it is allowed to do to
//! it.
//!
//! A handle is a *slot in one process's own table*. It designates nothing
//! outside that table: a number lifted out of another process's log, or counted
//! up from zero, resolves to that process's own slot or to nothing at all — so
//! holding a number is never the authority.
//!
//! **Only an owner ends, moves, duplicates or lends a handle.** The calls that
//! do (`close`, `message_send`'s handles, `dup`, `dup_narrowed`, `shm_sub`,
//! `vm_map`, `device_dma_map` and a spawn's endowment) are `unsafe`
//! here, since a safe type over the same slot (a mapping, an exclusive writer)
//! would otherwise be undone by a number. A call that only acts on the object
//! (`read`, `write`, `fstat`, a poll) stays safe over a number: misdirected, it
//! reaches the wrong object's protocol, not freed memory. Above this crate,
//! every API takes an owned or borrowed handle (`toyos::OwnedHandle`,
//! `toyos::BorrowedHandle`, `std::os::toyos::io`), so a raw number reaches it
//! only through a visible conversion.

crate::user_safe! {
    /// One entry in a process's handle table.
    ///
    /// Twelve bits of slot and twenty of generation, in one `u32` so it costs a
    /// register at the syscall boundary. **A slot at generation 0 encodes as the
    /// bare index**, which is what keeps stdio literally `0`, `1`, `2` and the std
    /// fork's stdio plumbing untouched.
    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
    pub struct RawHandle(pub u32);
}

impl RawHandle {
    const SLOT_BITS: u32 = 12;
    const SLOT_MASK: u32 = (1 << Self::SLOT_BITS) - 1;

    /// Slots one process may hold. Bounds the table; `MAX_HANDLES` is the name
    /// the kernel refuses by.
    pub const MAX_SLOTS: usize = 1 << Self::SLOT_BITS;

    /// **No table ever issues a handle at this generation.** A slot whose
    /// counter would step to it retires instead, permanently, so a spent slot
    /// is one the table no longer has rather than one whose numbers start
    /// again. Two things rest on it: an ancient handle can never come back to
    /// life, and [`HANDLE_INVALID`] — which encodes slot 4095 at this
    /// generation — is a number nothing holds.
    ///
    /// The last generation a slot is issued at is therefore `MAX_GENERATION - 1`
    /// (`kernel::object::handle`).
    pub const MAX_GENERATION: u32 = (1 << (32 - Self::SLOT_BITS)) - 1;

    /// A generation past [`MAX_GENERATION`](Self::MAX_GENERATION) loses its
    /// overflowing bits here rather than panicking, in every profile — which is
    /// why the kernel's table retires a slot instead of counting past the field
    /// and finding itself back at generation 0.
    pub const fn new(slot: u16, generation: u32) -> Self {
        Self((generation << Self::SLOT_BITS) | (slot as u32 & Self::SLOT_MASK))
    }

    pub const fn slot(self) -> u16 {
        (self.0 & Self::SLOT_MASK) as u16
    }

    /// Every process's handle to itself, carrying [`Rights::WRITE`],
    /// [`Rights::DUP`] and [`Rights::TRANSFER`]: what it hands on for a child
    /// to be placed under it (`SpawnArgs::place`). The first handle the
    /// kernel puts in a new table.
    pub const SELF: RawHandle = RawHandle::new(0, 0);

    /// Every process's notice, readable while a quit has asked it a reason
    /// it has not read, which a read takes (`SYS_PROCESS_QUIT`). The second
    /// handle the kernel puts in a new table.
    pub const NOTICE: RawHandle = RawHandle::new(1, 0);

    /// The `i`th handle a process was endowed at its start, in the order its
    /// spawn listed them (`SpawnArgs::endow_ptr`): a new table's handles
    /// follow [`SELF`](Self::SELF) and [`NOTICE`](Self::NOTICE) in that order.
    pub const fn endowed(i: usize) -> RawHandle {
        assert!(i < crate::syscall::MAX_SPAWN_ENDOWMENTS);
        RawHandle::new(2 + i as u16, 0)
    }

    pub const fn generation(self) -> u32 {
        self.0 >> Self::SLOT_BITS
    }
}

/// No handle. What an absent optional argument is spelled as, and never a slot
/// any table issues.
pub const HANDLE_INVALID: RawHandle = RawHandle(u32::MAX);

/// Every kind of kernel object a handle can name: the word a receive reports
/// beside each handle it installs ([`ReceivedEntry`]) and
/// [`CENSUS_KIND`](crate::syscall::debug_action::CENSUS_KIND) takes.
///
/// **A kind's word is ABI: it is never renumbered and never given to another
/// kind.** A new kind takes the next word, a kind that goes leaves its word
/// unused for good, as 8, the namespace's, is, and a receiver refuses a word it
/// does not know.
#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ObjectKind {
    PipeRead = 0,
    PipeWrite = 1,
    Connection = 2,
    Device = 3,
    Acceptor = 4,
    Inbox = 5,
    SharedMem = 6,
    Connector = 7,
    SysCap = 9,
    Process = 10,
    Null = 11,
    Job = 12,
    Notice = 13,
    WakeLatency = 14,
    EventSignal = 15,
    EventWait = 16,
    Pager = 17,
    Paged = 18,
    DeviceSet = 19,
}

impl ObjectKind {
    /// Every kind, in word order.
    pub const ALL: [ObjectKind; 19] = [
        Self::PipeRead,
        Self::PipeWrite,
        Self::Connection,
        Self::Device,
        Self::Acceptor,
        Self::Inbox,
        Self::SharedMem,
        Self::Connector,
        Self::SysCap,
        Self::Process,
        Self::Null,
        Self::Job,
        Self::Notice,
        Self::WakeLatency,
        Self::EventSignal,
        Self::EventWait,
        Self::Pager,
        Self::Paged,
        Self::DeviceSet,
    ];

    /// The kind `word` names: `None` for a word no kind has, a retired one's among them.
    pub const fn from_word(word: u32) -> Option<Self> {
        let mut i = 0;
        while i < Self::ALL.len() {
            if Self::ALL[i] as u32 == word {
                return Some(Self::ALL[i]);
            }
            i += 1;
        }
        None
    }
}

// `ALL` lists each kind once, in word order, and the namespace's word stays retired.
const _: () = {
    let mut i = 1;
    while i < ObjectKind::ALL.len() {
        assert!((ObjectKind::ALL[i - 1] as u32) < ObjectKind::ALL[i] as u32, "ObjectKind::ALL is out of word order");
        i += 1;
    }
    assert!(ObjectKind::from_word(8).is_none(), "word 8, the namespace's, was reassigned");
};

/// What the holder of a handle may do with the object behind it.
///
/// Rights only ever shrink: a duplicate may ask for a subset and nothing
/// widens. There is no default — every syscall states what it needs, because a
/// right left unstated is a right each call site invents.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rights(u32);

impl Rights {
    pub const NONE: Rights = Rights(0);
    /// May be duplicated. A [device claim] is created without it, which is what
    /// makes at most one handle to a claim expressible.
    ///
    /// [device claim]: crate::syscall::SYS_DEVICE_CLAIM
    pub const DUP: Rights = Rights(1 << 0);
    /// May be endowed at spawn or sent over a connection.
    pub const TRANSFER: Rights = Rights(1 << 1);
    pub const READ: Rights = Rights(1 << 2);
    /// On memory: map it writable, or lend it to a device; a holder without
    /// it maps the region only readable.
    pub const WRITE: Rights = Rights(1 << 3);
    /// Shared memory, an [inbox](crate::inbox)'s rings.
    pub const MAP: Rights = Rights(1 << 4);
    /// Block on it, or name it in an [`OP_WATCH`](crate::inbox::OP_WATCH).
    pub const WAIT: Rights = Rights(1 << 5);
    /// Kill a process.
    pub const MANAGE: Rights = Rights(1 << 6);
    /// On a `SysCap`: enter the RT band.
    pub const RT: Rights = Rights(1 << 7);
    /// On a `SysCap`: mint a device claim.
    pub const DEVICE: Rights = Rights(1 << 8);
    /// On a `SysCap`: read the whole machine's kernel log.
    ///
    /// [`SYS_LOG_READ`] answers every record every CPU wrote, which is every
    /// process's business and no process's right by default. `logkeeper` holds
    /// it because writing `/log` is its job and `test-runner` because a gate
    /// reads what the kernel said; no other program in any boot config does.
    ///
    /// [`SYS_LOG_READ`]: crate::syscall::SYS_LOG_READ
    pub const LOG: Rights = Rights(1 << 9);
    /// On a `SysCap`: power the machine off, or return it to firmware.
    ///
    /// [`SYS_SHUTDOWN`] and [`SYS_REBOOT`] each end every process there is and
    /// do not come back, so this is the largest authority this capability
    /// carries — one bit for both, because a machine taken away from its
    /// processes is the same authority whichever state it is left in. It rides a bit for
    /// the same reason minting a device claim and entering the real-time band
    /// do: what can cut the power is exactly what the supervisor endowed, and
    /// there is nothing a program can name to reach it otherwise.
    ///
    /// The kernel mints one carrying it, at boot, for the supervisor
    /// (`kernel::loader::spawn_supervisor`); every other holder is a narrowed
    /// duplicate the supervisor endowed from a `system.toml` row that named `power`.
    ///
    /// [`SYS_SHUTDOWN`]: crate::syscall::SYS_SHUTDOWN
    /// [`SYS_REBOOT`]: crate::syscall::SYS_REBOOT
    pub const POWER: Rights = Rights(1 << 10);
    /// On a `SysCap`: read the roster of every process in the machine.
    ///
    /// [`SYS_SYSINFO`] answers a header — total and used memory, the CPU count,
    /// the uptime — and then one entry per live thread carrying its pid, its
    /// scheduler state, its resident memory, its accumulated CPU time and its
    /// **name**. The header is a machine fact like [`SYS_CPU_COUNT`] and stays
    /// ambient; the entries are a census of what the machine is running, and a
    /// process that was endowed one connector has no business reading it.
    ///
    /// `toybox` holds it because `ps` is that binary under another
    /// name, and `test-runner` because several guest binaries read their own
    /// threads back out of the roster.
    ///
    /// [`SYS_SYSINFO`]: crate::syscall::SYS_SYSINFO
    /// [`SYS_CPU_COUNT`]: crate::syscall::SYS_CPU_COUNT
    pub const ROSTER: Rights = Rights(1 << 11);
    /// On a `SysCap`: read what the machine is made of, and who holds each
    /// part of it.
    ///
    /// [`SYS_DEVICE_INVENTORY`] answers one record per PCI function and one
    /// per claim naming the process that holds it. That is a census of the machine's hardware and of
    /// which program drives what, so it rides a bit as the roster does.
    ///
    /// [`SYS_DEVICE_INVENTORY`]: crate::syscall::SYS_DEVICE_INVENTORY
    pub const INVENTORY: Rights = Rights(1 << 12);
    /// On a `SysCap`: read the machine's counters.
    ///
    /// [`SYS_COUNTERS`] answers, per CPU, its own clock reading, how many
    /// times firmware took it over, `MSR_SMI_COUNT` where the CPU counts that,
    /// and the commands the kernel wrote to the firmware with the time they
    /// held the CPU:
    /// what the machine is doing, not what a program on it does. The counters
    /// that time programs need [`TRACE`](Self::TRACE) beside this.
    /// `inspect` holds it, and `test-runner`.
    ///
    /// [`SYS_COUNTERS`]: crate::syscall::SYS_COUNTERS
    pub const COUNTERS: Rights = Rights(1 << 13);
    /// On a `SysCap`: read what times the work of every other program.
    ///
    /// Beside [`COUNTERS`](Self::COUNTERS), [`SYS_COUNTERS`] answers each
    /// CPU's `APERF` and `MPERF` — its frequency and busy fraction, the
    /// Hertzbleed power channel — the package's energy and C-state
    /// residency and each AMD core's energy, the RAPL power channel
    /// (CVE-2020-8694), the kicks it took, the wake-ups that time keystrokes,
    /// and the power envelope it runs under. [`SYS_TRACE_READ`]
    /// answers, on this alone, when every thread in the machine was woken, ran
    /// and parked, and on which CPU. Admin tools hold it, `inspect`, `trace`
    /// and `test-runner`; no
    /// ordinary program and no toybox applet does while a manifest row grants
    /// a whole binary.
    ///
    /// [`SYS_COUNTERS`]: crate::syscall::SYS_COUNTERS
    /// [`SYS_TRACE_READ`]: crate::syscall::SYS_TRACE_READ
    pub const TRACE: Rights = Rights(1 << 14);
    /// On a `SysCap`: make a new job, with its own share of the CPU
    /// ([`SYS_JOB_CREATE`]). Every other process's children join their
    /// parent's job, so this is what an independently started program needs a
    /// launcher to hold — and what lets the holder gain aggregate CPU by
    /// starting many, which is why it rides a bit.
    ///
    /// [`SYS_JOB_CREATE`]: crate::syscall::SYS_JOB_CREATE
    pub const JOBS: Rights = Rights(1 << 15);
    /// On a `SysCap`: hold memory back for a job ([`SYS_JOB_FLOOR`]), which
    /// every other job is then refused. What it holds is protected capacity
    /// for the system's controls, so the supervisor alone holds it, and sets
    /// what a signed `system.toml` row declares.
    ///
    /// [`SYS_JOB_FLOOR`]: crate::syscall::SYS_JOB_FLOOR
    pub const FLOOR: Rights = Rights(1 << 16);
    /// On a `SysCap`: hold every idle CPU to waking within a bound
    /// ([`SYS_WAKE_LATENCY_HOLD`]). What it costs is the whole machine's
    /// power, every CPU kept out of its deeper states while the hold lives,
    /// which is why it rides a bit of its own and not [`RT`](Self::RT)'s:
    /// one takes CPU from other programs, this takes power from the machine.
    ///
    /// [`SYS_WAKE_LATENCY_HOLD`]: crate::syscall::SYS_WAKE_LATENCY_HOLD
    pub const WAKE_LATENCY: Rights = Rights(1 << 17);
    /// On a `SysCap` named by [`SpawnArgs::pager`]: start the child as a
    /// pager, which may make the one pager object that supplies paged
    /// objects ([`SYS_PAGER_CREATE`]). A pager holds every reader of its
    /// files to its answers, so the supervisor alone confers it, and only on
    /// a row the signed `system.toml` marks as one; no `syscap` name in a
    /// manifest maps to it.
    ///
    /// [`SpawnArgs::pager`]: crate::syscall::SpawnArgs::pager
    /// [`SYS_PAGER_CREATE`]: crate::syscall::SYS_PAGER_CREATE
    pub const PAGER: Rights = Rights(1 << 18);
    /// On a `SysCap`: say what the console's channels have delivered of the
    /// kernel's log ([`SYS_LOG_SPOKEN`]). What it says is kept off the UART
    /// by every fatal path and the stop, so it is not a reader's: the one
    /// program that feeds the console, `logkeeper`, holds it.
    ///
    /// [`SYS_LOG_SPOKEN`]: crate::syscall::SYS_LOG_SPOKEN
    pub const CONSOLE: Rights = Rights(1 << 19);
    /// On a `SysCap`: say the caller's last words before its end ends the
    /// machine ([`SYS_LOG_LAST_WORDS`]), one record every fatal path shows,
    /// once a boot. The supervisor's alone: the kernel mints it on the system
    /// capability, and no manifest's `syscap` name maps to it.
    ///
    /// [`SYS_LOG_LAST_WORDS`]: crate::syscall::SYS_LOG_LAST_WORDS
    pub const LAST_WORDS: Rights = Rights(1 << 20);

    /// Every bit that has a caller. A wider set than this is a bug in whoever
    /// composed it, not a right nobody uses.
    pub const ALL: Rights = Rights(0x1f_ffff);

    pub const fn from_bits(bits: u32) -> Option<Self> {
        if bits & !Self::ALL.0 == 0 { Some(Rights(bits)) } else { None }
    }

    pub const fn bits(self) -> u32 {
        self.0
    }

    pub const fn union(self, other: Rights) -> Rights {
        Rights(self.0 | other.0)
    }

    pub const fn without(self, other: Rights) -> Rights {
        Rights(self.0 & !other.0)
    }

    pub const fn contains(self, other: Rights) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn subset_of(self, of: Rights) -> bool {
        self.0 & !of.0 == 0
    }
}

crate::user_safe! {
    /// One handle a move takes out of its holder's table, by a message
    /// ([`message_send`]) or a spawn's endowment (`SpawnArgs::endow_ptr`), and what it
    /// carries where it arrives.
    ///
    /// The mover needs [`Rights::TRANSFER`] on it and names its own set or
    /// less: a set it does not hold is `PermissionDenied`, and a word that is
    /// no set `InvalidArgument`. A set without `TRANSFER` is how a holder is
    /// given a handle it cannot pass on.
    ///
    /// [`message_send`]: crate::syscall::message_send
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct TransferEntry {
        pub handle: RawHandle,
        /// A [`Rights`] set's bits, or [`Self::UNCHANGED`].
        pub rights: u32,
    }
}

impl TransferEntry {
    /// The rights word of a move that carries what the mover holds: no
    /// [`Rights`] set has these bits.
    pub const UNCHANGED: u32 = u32::MAX;

    /// `handle`, arriving with the rights it has.
    pub const fn unchanged(handle: RawHandle) -> Self {
        Self { handle, rights: Self::UNCHANGED }
    }

    /// `handle`, arriving with `rights` alone.
    pub const fn narrowed(handle: RawHandle, rights: Rights) -> Self {
        Self { handle, rights: rights.bits() }
    }

    /// The set the move asks for, `None` for the mover's own; `Err` for a
    /// word that is neither.
    pub const fn rights(self) -> Result<Option<Rights>, NotRights> {
        if self.rights == Self::UNCHANGED {
            return Ok(None);
        }
        match Rights::from_bits(self.rights) {
            Some(rights) => Ok(Some(rights)),
            None => Err(NotRights),
        }
    }
}

const _: () = assert!(Rights::from_bits(TransferEntry::UNCHANGED).is_none());

crate::user_safe! {
    /// One handle a message carried, as [`message_recv`] answers it: where
    /// it is in the receiver's table and what it names, read in the hold
    /// that installed it.
    ///
    /// [`message_recv`]: crate::syscall::message_recv
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct ReceivedEntry {
        pub handle: RawHandle,
        /// An [`ObjectKind`] word.
        pub kind: u32,
    }
}

impl ReceivedEntry {
    /// No handle, of no kind: what a receive's buffer holds before the kernel
    /// writes it.
    pub const NONE: Self = Self { handle: HANDLE_INVALID, kind: u32::MAX };

    /// What the handle names; `None` for a word this ABI does not know.
    pub const fn kind(self) -> Option<ObjectKind> {
        ObjectKind::from_word(self.kind)
    }
}

/// A rights word with a bit no right uses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NotRights;

/// Named bits rather than a number, because the one place this is read is a
/// refusal saying which right was missing.
impl core::fmt::Debug for Rights {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        const NAMES: [(Rights, &str); 21] = [
            (Rights::DUP, "DUP"),
            (Rights::TRANSFER, "TRANSFER"),
            (Rights::READ, "READ"),
            (Rights::WRITE, "WRITE"),
            (Rights::MAP, "MAP"),
            (Rights::WAIT, "WAIT"),
            (Rights::MANAGE, "MANAGE"),
            (Rights::RT, "RT"),
            (Rights::DEVICE, "DEVICE"),
            (Rights::LOG, "LOG"),
            (Rights::POWER, "POWER"),
            (Rights::ROSTER, "ROSTER"),
            (Rights::INVENTORY, "INVENTORY"),
            (Rights::COUNTERS, "COUNTERS"),
            (Rights::TRACE, "TRACE"),
            (Rights::JOBS, "JOBS"),
            (Rights::FLOOR, "FLOOR"),
            (Rights::WAKE_LATENCY, "WAKE_LATENCY"),
            (Rights::PAGER, "PAGER"),
            (Rights::CONSOLE, "CONSOLE"),
            (Rights::LAST_WORDS, "LAST_WORDS"),
        ];
        if self.0 == 0 {
            return f.write_str("NONE");
        }
        let mut first = true;
        for (right, name) in NAMES {
            if self.contains(right) {
                if !first {
                    f.write_str("|")?;
                }
                f.write_str(name)?;
                first = false;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_zero_is_the_bare_index() {
        for slot in [0u16, 1, 2, 4095] {
            assert_eq!(RawHandle::new(slot, 0).0, u32::from(slot));
        }
    }

    #[test]
    fn a_handle_answers_the_slot_and_generation_it_was_made_from() {
        let h = RawHandle::new(4095, RawHandle::MAX_GENERATION - 1);
        assert_eq!(h.slot(), 4095);
        assert_eq!(h.generation(), RawHandle::MAX_GENERATION - 1);
        assert_ne!(h, HANDLE_INVALID);
    }

    /// The one encoding a table must never issue, and the reason a slot is
    /// retired at `MAX_GENERATION` rather than wrapped.
    #[test]
    fn nothing_below_the_retirement_generation_can_be_the_invalid_handle() {
        assert_eq!(
            HANDLE_INVALID,
            RawHandle::new((RawHandle::MAX_SLOTS - 1) as u16, RawHandle::MAX_GENERATION)
        );
    }

    /// **A generation past the field wraps here silently, in every profile** —
    /// which is why retirement has to be a state the kernel's table keeps and
    /// can never be an overflow it would notice.
    ///
    /// `<<` checks its shift *amount* and never the value, so the top bits go
    /// without a panic even with debug assertions on. A counter stepped one past
    /// `MAX_GENERATION` is therefore not a large generation, and not a trapped
    /// one: it is generation 0 again, the very number every handle a slot was
    /// first issued at carries.
    #[test]
    fn a_generation_past_the_field_is_generation_zero_again() {
        for slot in [0u16, 900, 4095] {
            assert_eq!(
                RawHandle::new(slot, RawHandle::MAX_GENERATION + 1),
                RawHandle::new(slot, 0),
                "slot {slot} one past the last generation",
            );
        }
    }

    #[test]
    fn rights_only_shrink() {
        let full = Rights::READ.union(Rights::WRITE).union(Rights::DUP);
        assert!(Rights::READ.subset_of(full));
        assert!(full.subset_of(full));
        assert!(!Rights::MAP.subset_of(full));
        assert!(full.contains(Rights::READ.union(Rights::WRITE)));
        assert_eq!(full.without(Rights::DUP), Rights::READ.union(Rights::WRITE));
    }

    #[test]
    fn a_moves_rights_word_is_a_set_or_the_movers_own() {
        let h = RawHandle::new(3, 1);
        assert_eq!(TransferEntry::unchanged(h).rights(), Ok(None));
        assert_eq!(TransferEntry::narrowed(h, Rights::NONE).rights(), Ok(Some(Rights::NONE)));
        assert_eq!(TransferEntry::narrowed(h, Rights::MAP).rights(), Ok(Some(Rights::MAP)));
        assert_eq!(TransferEntry { handle: h, rights: Rights::ALL.bits() + 1 }.rights(), Err(NotRights));
    }

    #[test]
    fn a_bit_no_right_uses_is_not_a_rights_set() {
        assert!(Rights::from_bits(Rights::ALL.bits()).is_some());
        assert!(Rights::from_bits(Rights::ALL.bits() | (1 << 31)).is_none());
    }

    /// **`ALL` and the `Debug` table are two spellings of one set**, and a
    /// right added to one and not the other is invisible in both directions: a
    /// missing `ALL` bit makes `from_bits` refuse a legitimate set, and a
    /// missing name makes the one place a right is ever printed — a refusal
    /// saying which one was absent — omit it without saying so.
    ///
    /// The second assertion is the one that has teeth against a *future* right:
    /// an unnamed non-zero bit renders as the empty string rather than as
    /// anything a reader would question.
    #[test]
    fn every_right_in_all_has_a_name_and_every_name_is_in_all() {
        let named = |r: Rights| std::format!("{r:?}").split('|').count();
        assert_eq!(
            named(Rights::ALL),
            Rights::ALL.bits().count_ones() as usize,
            "a bit in ALL has no name in Debug: {:?}",
            Rights::ALL
        );
        assert_eq!(
            std::format!("{:?}", Rights(u32::MAX)),
            std::format!("{:?}", Rights::ALL),
            "a named right is outside ALL, so from_bits refuses a set that has it"
        );
        assert_eq!(std::format!("{:?}", Rights::LOG), "LOG");
    }
}
