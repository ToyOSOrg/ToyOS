// Syscall numbers (must match kernel dispatch table)
pub const SYS_WRITE: u64 = 0;
pub const SYS_READ: u64 = 1;
pub const SYS_THREAD_EXIT: u64 = 5;
pub const SYS_RANDOM: u64 = 6;
pub const SYS_OPEN: u64 = 9;
pub const SYS_CLOSE: u64 = 10;
pub const SYS_SEEK: u64 = 13;
pub const SYS_FSTAT: u64 = 14;
pub const SYS_FSYNC: u64 = 15;
/// What a path names, as a [`Stat`]: [`stat`].
pub const SYS_STAT: u64 = 16;
pub const SYS_READDIR: u64 = 17;
pub const SYS_DELETE: u64 = 18;
pub const SYS_SHUTDOWN: u64 = 19;
pub const SYS_CHDIR: u64 = 20;
pub const SYS_GETCWD: u64 = 21;
pub const SYS_PIPE: u64 = 24;
/// Start a program, endowing it exactly what the caller names. Answers a
/// `Process` handle — see [`spawn`].
pub const SYS_SPAWN: u64 = 25;
pub const SYS_MARK_TTY: u64 = 28;
pub const SYS_THREAD_SPAWN: u64 = 40;
pub const SYS_THREAD_JOIN: u64 = 41;
/// A header describing the machine, optionally followed by the roster of every
/// live thread in it. See [`sysinfo`].
///
/// **Two answers under one number, and only the second is authority.** The
/// header is a machine fact like [`SYS_CPU_COUNT`]; the entries name, size and
/// time every process there is, so writing one demands [`Rights::ROSTER`] on a
/// `SysCap`. Which of the two a call asks for is the buffer's own length: a
/// buffer with no room for an entry asks for the header, and the capability is
/// not consulted at all.
///
/// [`Rights::ROSTER`]: crate::handle::Rights::ROSTER
pub const SYS_SYSINFO: u64 = 45;

/// Bytes of machine header [`SYS_SYSINFO`] writes first: total memory, used
/// memory, the CPU count, the entry count, the uptime, and the busy and
/// available CPU nanoseconds a percentage is derived from.
///
/// **The one definition.** The kernel writes this layout and every reader
/// decodes it, so a second spelling is a reader that walks off by a field.
pub const SYSINFO_HEADER_SIZE: usize = 48;

/// [`SYSINFO_HEADER_SIZE`] bytes, decoded once: the one spelling of the
/// header's offsets, so a reader takes a field rather than an index.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SysinfoHeader {
    pub memory_total: u64,
    /// Bytes in use or promised to commitments not yet touched: what no
    /// new commitment can have.
    pub memory_used: u64,
    pub cpus: u32,
    /// Live threads in the roster this same call would answer with
    /// [`Rights::ROSTER`](crate::handle::Rights::ROSTER).
    pub entries: u32,
    pub uptime_ns: u64,
    pub total_cpu_ns: u64,
    pub total_available_ns: u64,
}

impl SysinfoHeader {
    pub fn decode(header: &[u8; SYSINFO_HEADER_SIZE]) -> Self {
        let u64_at = |at: usize| u64::from_le_bytes(header[at..at + 8].try_into().expect("eight"));
        let u32_at = |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().expect("four"));
        Self {
            memory_total: u64_at(0),
            memory_used: u64_at(8),
            cpus: u32_at(16),
            entries: u32_at(20),
            uptime_ns: u64_at(24),
            total_cpu_ns: u64_at(32),
            total_available_ns: u64_at(40),
        }
    }
}

/// Bytes per roster entry, after the header: pid, tid, scheduler state, whether
/// it is a secondary thread, resident memory, CPU nanoseconds, and a 28-byte
/// name.
pub const SYSINFO_ENTRY_SIZE: usize = 64;
pub const SYS_NANOSLEEP: u64 = 49;
/// A second handle to the same object, carrying no more than the first. See
/// [`dup`] and [`dup_narrowed`].
pub const SYS_HANDLE_DUP: u64 = 50;
pub const SYS_GETPID: u64 = 51;
pub const SYS_RENAME: u64 = 52;
pub const SYS_MKDIR: u64 = 53;
pub const SYS_RMDIR: u64 = 54;
pub const SYS_DLOPEN: u64 = 55;
pub const SYS_DLSYM: u64 = 56;
pub const SYS_DLCLOSE: u64 = 57;
pub const SYS_FUTEX_WAIT: u64 = 58;
pub const SYS_FUTEX_WAKE: u64 = 59;
pub const SYS_FTRUNCATE: u64 = 60;
pub const SYS_STACK_INFO: u64 = 61;
pub const SYS_CPU_COUNT: u64 = 62;
pub const SYS_VM_RESERVE: u64 = 63;
pub const SYS_VM_RELEASE: u64 = 64;
pub const SYS_READ_NONBLOCK: u64 = 66;
pub const SYS_WRITE_NONBLOCK: u64 = 67;
pub const SYS_EXIT: u64 = 72;
pub const SYS_GET_ENV: u64 = 73;
pub const SYS_CLOCK_EPOCH: u64 = 75;
pub const SYS_SYMLINK: u64 = 81;
pub const SYS_READLINK: u64 = 82;
/// Accept a queued connection from an [`Acceptor`] handle.
///
/// [`Acceptor`]: crate::handle::RawHandle
pub const SYS_ACCEPT: u64 = 86;
/// Allocate a TLS block for a dlopen'd module on the current thread.
/// Arg0: module_id (1-based DTV index). Returns the block's virtual address,
/// or a `SyscallError` word — see [`tls_alloc_block`].
pub const SYS_TLS_ALLOC_BLOCK: u64 = 88;
/// Create an [`inbox`](crate::inbox) and map its rings. See [`inbox_setup`].
pub const SYS_INBOX_SETUP: u64 = 89;
/// Hand queued submissions to the kernel and/or wait for completions. See
/// [`inbox_submit`].
pub const SYS_INBOX_SUBMIT: u64 = 90;
pub const SYS_QUERY_MODULES: u64 = 91;
/// Debug syscall. Arg0 selects the action:
///   0 = kernel panic (triggers panic!() in syscall context)
///   1 = kernel fault (null pointer deref in kernel context)
pub const SYS_DEBUG: u64 = 92;
/// Make a new job — the owner its threads share CPU time under — gated by
/// [`Rights::JOBS`] on a `SysCap`. See [`job_create`].
///
/// [`Rights::JOBS`]: crate::handle::Rights::JOBS
pub const SYS_JOB_CREATE: u64 = 93;
pub const SYS_PROCESS_STATS: u64 = 94;
pub const SYS_SET_THREAD_NAME: u64 = 95;
/// Read one register of a claimed PCI function's config space. See
/// [`device_reg_read`].
pub const SYS_DEVICE_REG_READ: u64 = 97;

/// Read this process's endowment table back: the `(label, handle)` pairs its
/// parent gave it at spawn, as an `[EndowEntry]` count followed by the entries
/// and the label blob they index into. `buf_len == 0` asks how many bytes the
/// answer needs. See [`endowments`].
///
/// The handles themselves are in the table whether or not this is ever called —
/// the labels are *names* for them, not the authority.
pub const SYS_ENDOWMENTS: u64 = 99;

/// Make a port: one [`Acceptor`] for the server, one `Connector` for its
/// clients, packed `(acceptor << 32) | connector`. See [`port_create`].
///
/// **The packing cannot be read as an error, and the reason is slot
/// retirement.** A `SyscallError` encodes as `u64::MAX - code` for
/// `code < 256`, so a pair could collide only if both halves could reach
/// `0xFFFF_FFFF`. A slot at [`RawHandle::MAX_GENERATION`] is retired rather
/// than reissued, so the largest handle any table hands out is `0xFFFF_EFFF`
/// and the largest pair is `0xFFFF_EFFF_FFFF_EFFF` — four billion below the
/// error range. The retirement rule and this packing are load-bearing for each
/// other.
///
/// [`Acceptor`]: port_create
pub const SYS_PORT_CREATE: u64 = 100;
/// Build a namespace from a base and a set of `(name, connector)` additions.
/// See [`NamespaceBuild`].
pub const SYS_NAMESPACE_BUILD: u64 = 101;
/// Open a connection to a name **in a namespace this process holds**. There is
/// no other place to ask, and a name it was not given resolves to nothing.
pub const SYS_NAMESPACE_OPEN: u64 = 102;

/// Make a shared-memory region, sized up to whole [`PAGE_SIZE`]s.
/// See [`shm_create`].
pub const SYS_SHM_CREATE: u64 = 105;
/// Map a region into the caller. See [`shm_map`].
pub const SYS_SHM_MAP: u64 = 106;

/// The page every mapping is made of, and what [`SYS_SHM_CREATE`] rounds a
/// length up to a whole number of.
pub const PAGE_SIZE: usize = 4096;

/// [`shm_create_flags`]: memory a device can be lent, in whole
/// [`SHM_DEVICE_PAGE`]s of one physical run, as a device's domain maps it.
pub const SHM_DEVICE: u64 = 1;

/// [`shm_create_for`]: the region is counted to the job the call's third
/// argument names, a handle carrying `WRITE`, rather than the caller's: the
/// authority to place a child in a job is the authority to make memory for it.
pub const SHM_FOR_JOB: u64 = 1 << 1;

/// What a [`SHM_DEVICE`] region is a whole number of.
pub const SHM_DEVICE_PAGE: u64 = 2 * 1024 * 1024;

/// Wait for the process a handle names and take how it ended, an [`End`]'s
/// word, gated by [`Rights::WAIT`]. See [`process_wait`].
///
/// **An end is a property of the object, not of a table entry**, so this
/// answers whether or not the process is still around: a spawner that waits a
/// second time gets the same end, and one that waits for the first time long
/// after the process is gone gets it too. There is nothing to reap and no
/// window in which an end is missed.
///
/// [`Rights::WAIT`]: crate::handle::Rights::WAIT
pub const SYS_PROCESS_WAIT: u64 = 108;
/// Kill the process a handle names, gated by [`Rights::MANAGE`].
/// See [`process_kill`].
///
/// [`Rights::MANAGE`]: crate::handle::Rights::MANAGE
pub const SYS_PROCESS_KILL: u64 = 109;

/// Mint a device claim for a class, gated by [`Rights::DEVICE`] on a `SysCap`.
/// Only the supervisor holds such a cap, so the set of processes that can ever
/// claim a device is exactly what the supervisor endowed. See [`device_claim`].
///
/// [`Rights::DEVICE`]: crate::handle::Rights::DEVICE
pub const SYS_DEVICE_CLAIM: u64 = 111;
/// Enter the real-time scheduling band, gated by [`Rights::RT`] on a `SysCap`.
/// A claim is not a privilege; this is. See [`rt_enter`].
///
/// [`Rights::RT`]: crate::handle::Rights::RT
pub const SYS_RT_ENTER: u64 = 112;

/// Copy kernel log records into a caller's buffer, advancing a cursor the
/// caller owns. Gated by [`Rights::LOG`] on a `SysCap`. See [`log_read`] and
/// [`crate::log`].
///
/// **The kernel keeps no per-reader state**, so a second reader costs nothing
/// and the stream is not consumed: `logkeeper` and a `log-follow` tool coexist
/// with no coordination. Reading the whole machine's log is authority, which is
/// why it rides a right rather than being ambient.
///
/// [`Rights::LOG`]: crate::handle::Rights::LOG
pub const SYS_LOG_READ: u64 = 114;

/// Return the machine to firmware, on the same `Rights::POWER` that powers it off. See [`reboot`].
pub const SYS_REBOOT: u64 = 116;

/// One memory BAR of a claimed PCI function, as an object to map. See
/// [`device_bar_map`].
pub const SYS_DEVICE_BAR_MAP: u64 = 117;

/// Memory a claimed PCI function may reach, and nothing else may. See
/// [`device_dma_alloc`].
pub const SYS_DEVICE_DMA_ALLOC: u64 = 118;

// 119 and 120 were `SYS_PARTITION_READ` and `SYS_PARTITION_WRITE`, a
// partition claim's blocks on a disk this kernel drove; it drives none.

/// What the machine is made of, and who holds each part of it, as
/// [`crate::inventory`] records. Gated by [`Rights::INVENTORY`] on a `SysCap`.
/// See [`device_inventory`].
///
/// [`Rights::INVENTORY`]: crate::handle::Rights::INVENTORY
pub const SYS_DEVICE_INVENTORY: u64 = 121;

/// Put a region the caller holds into a claimed PCI function's address space
/// at the unit, so the function reaches it as well as the caller's own grants.
/// See [`device_dma_map`].
pub const SYS_DEVICE_DMA_MAP: u64 = 122;

/// Take a region [`SYS_DEVICE_DMA_MAP`] put there back out. See
/// [`device_dma_unmap`].
pub const SYS_DEVICE_DMA_UNMAP: u64 = 123;

/// Every online CPU's counters, as [`crate::counters`] records. Gated by
/// [`Rights::COUNTERS`] on a `SysCap`, and the counters that time programs and
/// the power envelope by [`Rights::TRACE`] beside it. See [`counters`].
///
/// [`Rights::COUNTERS`]: crate::handle::Rights::COUNTERS
/// [`Rights::TRACE`]: crate::handle::Rights::TRACE
pub const SYS_COUNTERS: u64 = 124;

/// Make a connector to a port the caller accepts on, every connection through
/// which is stamped with the badge it carries. Gated by `READ` on the
/// acceptor, the right accepting takes. See [`port_mint`].
pub const SYS_PORT_MINT: u64 = 125;

/// Read the badge a connection accepted from the caller's port was stamped
/// with. See [`port_badge`].
pub const SYS_PORT_BADGE: u64 = 126;

/// The diary's records the caller's cursor has not seen, as
/// [`crate::trace`] records. Gated by [`Rights::TRACE`] on a `SysCap`. See
/// [`trace_read`].
///
/// [`Rights::TRACE`]: crate::handle::Rights::TRACE
pub const SYS_TRACE_READ: u64 = 127;

/// One operation on the `acpi` claim ([`crate::acpi::op`]): an access the
/// kernel mediates, the firmware's Global Lock, or the power-off's sleep type.
/// See [`acpi_access`], [`acpi_lock_take`] and [`acpi_s5`].
pub const SYS_ACPI: u64 = 128;
/// A fresh null handle: every read answers end of file and every write takes
/// all it is given. See [`null`].
pub const SYS_NULL: u64 = 130;
/// State the address width a claimed PCI function issues. See
/// [`device_dma_limit`].
pub const SYS_DEVICE_DMA_LIMIT: u64 = 131;
/// Let a claimed PCI function master the bus. See [`device_dma_start`].
pub const SYS_DEVICE_DMA_START: u64 = 132;
/// Queue a whole message on a connection, its bytes and the handles it
/// carries, or nothing of it. See [`message_send`].
pub const SYS_MESSAGE_SEND: u64 = 133;
/// A handle to the job the caller runs in. See [`job_own`].
pub const SYS_JOB_OWN: u64 = 134;
/// Ask the process a handle names, and every process below it, to quit, for
/// a [`crate::quit::Reason`], gated by [`Rights::MANAGE`] as a kill is. See
/// [`process_quit`].
///
/// [`Rights::MANAGE`]: crate::handle::Rights::MANAGE
pub const SYS_PROCESS_QUIT: u64 = 135;
/// Whether [`SYS_SHUTDOWN`] or [`SYS_REBOOT`] would stop this machine, asked
/// on the same right and stopping nothing. See [`machine_stops`].
pub const SYS_MACHINE_STOPS: u64 = 136;
/// Where firmware loaded this boot from, as the loader handed it. Gated by
/// [`Rights::INVENTORY`] on a `SysCap`. See [`boot_sources`].
///
/// [`Rights::INVENTORY`]: crate::handle::Rights::INVENTORY
pub const SYS_BOOT_SOURCES: u64 = 137;
/// Read a kernel file at an offset, leaving its handle's offset where it is.
/// See [`pread`].
pub const SYS_PREAD: u64 = 138;
/// Write a kernel file at an offset, whether or not its handle appends, and
/// leaving the handle's offset where it is. See [`pwrite`].
pub const SYS_PWRITE: u64 = 139;
pub const SYS_VM_PROTECT: u64 = 140;
pub const SYS_VM_DISCARD: u64 = 141;
pub const SYS_VM_DECOMMIT: u64 = 142;
/// A memory object over part of another's pages. See [`shm_sub`].
pub const SYS_SHM_SUB: u64 = 143;
/// Hold memory back for a job however short every other runs, gated by
/// [`Rights::FLOOR`] on a `SysCap`. See [`job_floor`].
///
/// [`Rights::FLOOR`]: crate::handle::Rights::FLOOR
pub const SYS_JOB_FLOOR: u64 = 144;
/// What a job's account holds. See [`account_info`].
pub const SYS_ACCOUNT_INFO: u64 = 145;
/// Resize a run of committed private memory, in place or moved. See [`vm_remap`].
pub const SYS_VM_REMAP: u64 = 146;
/// Hold every idle CPU to waking within a bound, gated by
/// [`Rights::WAKE_LATENCY`] on a `SysCap`. See [`wake_latency_hold`].
///
/// [`Rights::WAKE_LATENCY`]: crate::handle::Rights::WAKE_LATENCY
pub const SYS_WAKE_LATENCY_HOLD: u64 = 147;
/// A fresh event's two sides. See [`event_create`].
pub const SYS_EVENT_CREATE: u64 = 148;
/// Raise an event, on its signal side's `WRITE`. See [`event_signal`].
pub const SYS_EVENT_SIGNAL: u64 = 149;
/// Take an event's pending signal, on its wait side's `READ`. See [`event_take`].
pub const SYS_EVENT_TAKE: u64 = 150;
/// Park on one handle's one condition, with a timeout. See [`handle_wait`].
pub const SYS_HANDLE_WAIT: u64 = 151;
/// The clock now, read by the kernel. See [`clock_now`].
pub const SYS_CLOCK_NOW: u64 = 152;
/// Make this process's one pager object. See [`pager_create`].
pub const SYS_PAGER_CREATE: u64 = 153;
/// A paged object a pager supplies. See [`paged_create`].
pub const SYS_PAGED_CREATE: u64 = 154;
/// A holder handle on a paged object. See [`paged_holder`].
pub const SYS_PAGED_HOLDER: u64 = 155;
/// Read a paged object. See [`paged_read`].
pub const SYS_PAGED_READ: u64 = 156;
/// An owner's call on its paged object. See [`PagedOp`].
pub const SYS_PAGED_OP: u64 = 157;
/// What a pager's objects ask of it. See [`paged_notices`].
pub const SYS_PAGED_NOTICES: u64 = 158;
/// Take a connection's next whole message. See [`message_recv`].
pub const SYS_MESSAGE_RECV: u64 = 159;
/// Whether a console holder carries the log off the machine. See [`console_carried`].
pub const SYS_CONSOLE_CARRIED: u64 = 160;
/// Set the CPUs one of the caller's own threads may run on. See [`thread_set_affinity`].
pub const SYS_THREAD_SET_AFFINITY: u64 = 161;
/// Read them back into a buffer of at least [`CPU_MASK_WORDS`] words, or
/// `InvalidArgument`. See [`thread_affinity`].
pub const SYS_THREAD_GET_AFFINITY: u64 = 162;
/// The CPU the caller is running on. See [`current_cpu`].
pub const SYS_CURRENT_CPU: u64 = 163;

/// Bins in the per-process syscall profile — one for every number this ABI
/// issues, and one at the end for every number it does not.
///
/// **The profile's parts sum to its total, and that is the whole requirement.**
/// A bin array narrower than the ABI reaches drops calls out of the line while
/// the total goes on counting them.
pub const SYSCALL_PROFILE_BINS: usize = 165;

/// Where a number this ABI does not issue is counted. Merging is a degradation
/// a reader can see in the line; dropping is one nobody can.
pub const SYSCALL_PROFILE_OTHER: usize = SYSCALL_PROFILE_BINS - 1;

const _: () = assert!(SYS_CURRENT_CPU < SYSCALL_PROFILE_OTHER as u64);

pub const WNOHANG: u64 = 1;
/// Every bit `SYS_PROCESS_WAIT`'s flag word defines; a wait carrying another
/// is refused `InvalidArgument`.
pub const PROCESS_WAIT_FLAGS_KNOWN: u64 = WNOHANG;

crate::user_safe! {
    /// Arguments for the `SYS_SPAWN` syscall, passed as a single pointer.
    ///
    /// **Every handle a child starts with is endowed, and an endowment moves**,
    /// so a parent that wants to keep what it endows duplicates first. That is
    /// what makes endowing a device claim work with no special case: a claim
    /// carries no [`Rights::DUP`], so the move is the only expressible form and
    /// the parent provably no longer holds it. A child's descriptors are
    /// endowments labelled [`FD_PREFIX`] and their number; a console among
    /// them is the child's own, minted for it.
    ///
    /// [`Rights::DUP`]: crate::handle::Rights::DUP
    #[derive(Clone, Copy)]
    pub struct SpawnArgs {
        /// The program: the file the kernel opens and pages the child from, or,
        /// with an `image`, the path the caller read that from. **Never `argv[0]`**, which is the child's first argument
        /// and the name it goes by, and which nothing opens.
        pub path_ptr: u64,
        pub path_len: u64,
        pub argv_ptr: u64,
        pub argv_len: u64,
        pub env_ptr: u64,
        pub env_len: u64,
        /// `[EndowEntry]`, moved out of the parent's table.
        pub endow_ptr: u64,
        pub endow_count: u64,
        /// The label blob every [`EndowEntry`]'s `label_off`/`label_len` indexes.
        pub labels_ptr: u64,
        pub labels_len: u64,
        /// The child's working directory, absolute, or the spawn is refused
        /// `InvalidArgument`. Under the name the kernel serves (`/tmp`) it
        /// must be a directory there or the spawn is `NotFound`; under any other
        /// name it is a file server's directory, which the caller's client asked
        /// that server about and the kernel cannot. **Always the caller's
        /// statement**: the kernel never substitutes the caller's own.
        pub cwd_ptr: u64,
        pub cwd_len: u64,
        /// The program's bytes, in a memory object the caller holds — its file
        /// server's for the file, or one the caller read the program into:
        /// `image` is a handle to it carrying `MAP`, and `image_len` how many of
        /// its first bytes the program is — or 0, for a program the kernel opens
        /// at `path` itself. `PermissionDenied` for a handle without `MAP`;
        /// `InvalidArgument` for a length the object does not hold, or a device's
        /// window. The object stays the caller's: what the kernel reads of it,
        /// and when, is `kernel/src/file_backing.rs`'s (`SharedImage`). `path` is
        /// then opened by nobody.
        pub image: u64,
        pub image_len: u64,
        /// The process the child is placed under, whose end takes it down: a
        /// handle carrying [`Rights::WRITE`] to it — a copy of that process's
        /// [`SELF_LABEL`] — or [`HANDLE_INVALID`] for the caller itself.
        /// `PermissionDenied` for a handle without `WRITE` and `InvalidArgument`
        /// for one to no process, since a place is a handle a peer sent;
        /// `Gone` for a process being torn down, and `ResourceExhausted` for a
        /// child more than `toyos_proclife::MAX_DEPTH` below the supervisor.
        ///
        /// [`Rights::WRITE`]: crate::handle::Rights::WRITE
        pub place: u64,
        /// A `SysCap` handle carrying [`Rights::PAGER`], which starts the
        /// child as a pager for its life, or [`HANDLE_INVALID`] for none.
        /// `PermissionDenied` for a handle without the right. The handle stays
        /// the caller's.
        ///
        /// [`Rights::PAGER`]: crate::handle::Rights::PAGER
        pub pager: u64,
    }
}

crate::user_safe! {
    /// Arguments for the `SYS_DLOPEN` syscall, passed as a single pointer.
    #[derive(Clone, Copy)]
    pub struct DlopenArgs {
        /// The name the module goes by: what `SYS_QUERY_MODULES` answers for
        /// it, and what a second load is known by, which answers the module
        /// already loaded and runs no initialiser again. Nothing opens it.
        pub name_ptr: u64,
        pub name_len: u64,
        /// The library's bytes, in a memory object as [`SpawnArgs::image`] names a
        /// program's: a handle carrying `MAP`, and how many of its first bytes
        /// the library is.
        pub image: u64,
        pub image_len: u64,
        /// Where the kernel writes the module's `[.init_array address, count]`,
        /// or 0 for nowhere.
        pub init_out: u64,
    }
}

/// One `(label, handle)` pair of a process's endowment table.
///
/// `label_off`/`label_len` index the label blob that travels beside the
/// entries — in [`SpawnArgs`] going in, in [`endowments`]'s answer coming back.
/// The label is a *local name* in one process's own table and buys nothing to
/// guess: a name not in your table resolves to nothing.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct EndowEntry {
    pub label_off: u32,
    pub label_len: u32,
    pub handle: RawHandle,
    /// Named, so nothing leaks kernel stack into it.
    pub _pad: u32,
}

const _: () = assert!(core::mem::size_of::<EndowEntry>() == 16);

/// The label the kernel puts on the supervisor's system capability, and the one
/// the supervisor puts on the `RT`-only dup it endows a `realtime` program.
///
/// Here rather than in the SDK because the kernel writes it and userland reads
/// it, and a label spelled twice is a label that can be spelled two ways.
pub const SYSCAP_LABEL: &str = "syscap";
/// The label for a program's namespace — what its manifest `receives` becomes.
pub const SVC_LABEL: &str = "svc";
/// The labels the kernel puts on the immutable memory objects it hands the
/// supervisor beside the system capability: the file server's executable and
/// ROOT's bytes, both as the loader verified them (`boot::KernelArgs`).
pub const FILESERVER_IMAGE_LABEL: &str = "boot:fileserver";
pub const ROOT_IMAGE_LABEL: &str = "boot:root";
/// The label every process starts holding a handle to itself under, carrying
/// `WRITE`, `DUP` and `TRANSFER`: what it hands on for a child to be placed
/// under it ([`SpawnArgs::place`]). The kernel puts it in every table.
pub const SELF_LABEL: &str = "self";
/// The label every process starts holding its notice under: readable while a
/// quit has asked it a reason it has not read, which a read takes
/// ([`SYS_PROCESS_QUIT`]). The kernel puts it in every table.
pub const QUIT_LABEL: &str = "quit";
/// The label a spawn endows a job under to put its child in it: the owner its
/// threads, and its children's unless a spawn names another, share CPU time
/// under. It takes a handle carrying [`Rights::WRITE`] to a job — the caller's
/// own is [`job_own`]'s, and a new one [`job_create`]'s. A spawn naming none
/// puts the child in its spawner's job.
///
/// [`Rights::WRITE`]: crate::handle::Rights::WRITE
pub const JOB_LABEL: &str = "job";
/// `serve:<name>`: the acceptor of a machine-wide port this program serves.
pub const SERVE_PREFIX: &str = "serve:";
/// `dev:<class>`: the claim for a device class this program was given.
pub const DEV_PREFIX: &str = "dev:";
/// `fd:<n>`: the child's descriptor `n`, which its descriptor table installs
/// at that number (`toyos::fd`). Numbering is userland's: the kernel only
/// moves the handle.
pub const FD_PREFIX: &str = "fd:";
/// `provide:<name>`: a connector a *launching client* transferred, beside the
/// namespace entry the launcher made from it.
///
/// **Both, and the second is what makes the chain work.** A namespace answers
/// with connections, not with connectors, so a process holding `surface` only
/// in its namespace cannot hand `surface` to a child — and the terminal → shell
/// → `locale` chain is exactly that. The connector arrives labelled as well, so
/// the holder can pass it on and the child's own manifest row still decides
/// everything else it gets.
pub const PROVIDE_PREFIX: &str = "provide:";

/// `(label, handle)` pairs one endowment table holds, the kernel's own
/// [`SELF_LABEL`] and [`QUIT_LABEL`] among them. Policy on the primitive,
/// refused by name, never truncated — the widest manifest row plus stdio.
pub const MAX_ENDOWMENTS: usize = 32;
/// `(label, handle)` pairs one spawn may carry: the kernel adds [`SELF_LABEL`]
/// and [`QUIT_LABEL`].
pub const MAX_SPAWN_ENDOWMENTS: usize = MAX_ENDOWMENTS - 2;
/// Bytes of label blob one endowment table holds, the kernel's labels among them.
pub const MAX_LABELS_LEN: usize = 4096;
/// Bytes of label blob one spawn may carry: the kernel adds its two labels.
pub const MAX_SPAWN_LABELS_LEN: usize = MAX_LABELS_LEN - SELF_LABEL.len() - QUIT_LABEL.len();

use crate::end::End;
use crate::handle::Rights;
use crate::pci::{DmaGrant, DmaMapping};
use crate::{Pid, RawHandle, HANDLE_INVALID};

/// Syscall error with a specific code. Values occupy the top of the u64 range:
/// error code N is encoded as `u64::MAX - N`. Any return value `>= u64::MAX - 255`
/// is an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
pub enum SyscallError {
    Unknown = 0,
    NotFound = 1,
    PermissionDenied = 2,
    AlreadyExists = 3,
    InvalidArgument = 4,
    BadAddress = 5,
    WouldBlock = 6,
    ResourceExhausted = 7,
    NotSupported = 8,
    /// The device did not do it.
    ///
    /// One word for a transfer that was issued and not completed, and for a
    /// volume whose own structures do not decode: there is nothing a caller can
    /// do differently about the two, and both are the opposite of `NotFound`.
    /// The channel below it — `block::BlockDevice`, `FileBacking::read_page`,
    /// `bcachefs::BlockIO`, `vfs::FileSystem` — is fallible the whole way so
    /// that this is what arrives, rather than "no such file".
    ///
    /// It carries nothing. Which endpoint stalled, what the sense key was and
    /// which block was asked for are in the kernel's own log line, where a
    /// triage reads them; an enum here would be a vocabulary userland has no
    /// use for and every new driver would have to guess an arm from.
    Io = 9,
    /// The object was there and its other end is not.
    ///
    /// **A different fact from `NotFound`, and the design does not work without
    /// the difference.** "The name is not in the namespace this process was
    /// given" is a statement about this process and the answer is "you have a
    /// bug"; "the server exited" is a statement about the machine. The SDK sees
    /// one `u64`, so if the kernel gives one word the SDK has one answer — and
    /// the same rule the storage layer already obeys applies here: a dead peer
    /// must not be indistinguishable from a handle that was never there.
    Gone = 10,
}

impl SyscallError {
    pub const fn to_u64(self) -> u64 {
        u64::MAX - self as u64
    }

    pub fn from_u64(val: u64) -> Option<Self> {
        if val < u64::MAX - 255 {
            return None;
        }
        let code = u64::MAX - val;
        match code {
            0 => Some(Self::Unknown),
            1 => Some(Self::NotFound),
            2 => Some(Self::PermissionDenied),
            3 => Some(Self::AlreadyExists),
            4 => Some(Self::InvalidArgument),
            5 => Some(Self::BadAddress),
            6 => Some(Self::WouldBlock),
            7 => Some(Self::ResourceExhausted),
            8 => Some(Self::NotSupported),
            9 => Some(Self::Io),
            10 => Some(Self::Gone),
            _ => Some(Self::Unknown),
        }
    }
}

impl core::fmt::Display for SyscallError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unknown => f.write_str("unknown error"),
            Self::NotFound => f.write_str("not found"),
            Self::PermissionDenied => f.write_str("permission denied"),
            Self::AlreadyExists => f.write_str("already exists"),
            Self::InvalidArgument => f.write_str("invalid argument"),
            Self::BadAddress => f.write_str("bad address"),
            Self::WouldBlock => f.write_str("would block"),
            Self::ResourceExhausted => f.write_str("resource exhausted"),
            Self::NotSupported => f.write_str("not supported"),
            Self::Io => f.write_str("the device did not complete the transfer"),
            Self::Gone => f.write_str("the other end is gone"),
        }
    }
}

/// Check a raw syscall return value: if it's an error, return Err; otherwise Ok(val).
fn check(val: u64) -> Result<u64, SyscallError> {
    match SyscallError::from_u64(val) {
        Some(e) => Err(e),
        None => Ok(val),
    }
}

/// Check a raw syscall return for success (0) or error.
fn check_unit(val: u64) -> Result<(), SyscallError> {
    check(val).map(|_| ())
}

/// File type for file handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u64)]
pub enum FileType {
    #[default]
    Unknown = 0,
    File = 1,
    Pipe = 2,
    // 3 was `Keyboard` and 7 `Mouse`, the kernel's input streams: input is
    // userland's, over the `input` port.
    Serial = 4,
    Framebuffer = 5,
    Tty = 6,
    Socket = 8,
    // 9 was `Nic`, the type a claim on the kernel's own NIC driver reported.
    // A claim on a PCI function reports `Unknown`: what it is, is the driver's
    // to know, and this kernel does not know it.
    /// A shared-memory region of memory the kernel allocated to it and maps
    /// write-back, as [`shm_create`] makes; `size` is its length. A program's
    /// stdout and stderr are one when they are its log ring (`toyos::log`).
    SharedMemory = 10,
    /// A directory, which only [`stat`] answers.
    Directory = 11,
    /// A symbolic link, which only [`stat`] answers; `size` is its target's length.
    Symlink = 12,
    /// Any other shared-memory region, such as a device's registers; `size`
    /// is its length. A peer's buffer is never one.
    DeviceMemory = 13,
    /// A [null](null) handle.
    Null = 14,
    /// A region nobody can write, which every mapping of is read-only: a
    /// payload the loader verified (`boot::BootImage`); `size` is the
    /// payload's length, within the region's pages.
    ImmutableMemory = 15,
}

impl FileType {
    pub fn from_u64(val: u64) -> Option<Self> {
        match val {
            0 => Some(Self::Unknown),
            1 => Some(Self::File),
            2 => Some(Self::Pipe),
            4 => Some(Self::Serial),
            5 => Some(Self::Framebuffer),
            6 => Some(Self::Tty),
            8 => Some(Self::Socket),
            10 => Some(Self::SharedMemory),
            11 => Some(Self::Directory),
            12 => Some(Self::Symlink),
            13 => Some(Self::DeviceMemory),
            14 => Some(Self::Null),
            15 => Some(Self::ImmutableMemory),
            _ => None,
        }
    }
}

/// Seek position for [`seek`].
pub enum SeekFrom {
    Start(u64),
    Current(i64),
    End(i64),
}

/// Flags for [`open`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenFlags(pub u64);

impl OpenFlags {
    pub const READ: Self = Self(1);
    pub const WRITE: Self = Self(2);
    pub const CREATE: Self = Self(4);
    pub const TRUNCATE: Self = Self(8);
    pub const APPEND: Self = Self(16);
    /// With [`Self::CREATE`] only: refused `AlreadyExists` when the name is
    /// there, a link or a directory included, decided in the one call that
    /// creates it.
    pub const EXCLUSIVE: Self = Self(32);
    /// Every bit defined above. An open carrying another is refused
    /// `InvalidArgument` before its path is resolved.
    pub const KNOWN: Self =
        Self(Self::READ.0 | Self::WRITE.0 | Self::CREATE.0 | Self::TRUNCATE.0 | Self::APPEND.0 | Self::EXCLUSIVE.0);

    pub const fn contains(self, flag: Self) -> bool { self.0 & flag.0 != 0 }
}

impl core::ops::BitOr for OpenFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self { Self(self.0 | rhs.0) }
}

impl core::ops::BitOrAssign for OpenFlags {
    fn bitor_assign(&mut self, rhs: Self) { self.0 |= rhs.0; }
}

/// What pages of a process's own memory may be used for. No value is both
/// writable and executable. `None` pages fault on any access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
pub enum VmProt {
    None = 0,
    Read = 1,
    ReadWrite = 2,
    ReadExec = 3,
}

impl VmProt {
    /// The protection a syscall argument names, `None` for a word that names none.
    pub const fn from_u64(word: u64) -> Option<Self> {
        Some(match word {
            0 => Self::None,
            1 => Self::Read,
            2 => Self::ReadWrite,
            3 => Self::ReadExec,
            _ => return None,
        })
    }
}

/// Where [`vm_reserve`] places a range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmAt {
    /// Wherever the space has room, at a multiple of `align`: a power of two,
    /// and a page at least.
    Anywhere { align: usize },
    /// Exactly there, on a page, in place of whatever those pages held.
    Fixed(*mut u8),
}

impl VmAt {
    /// Anywhere, on a page.
    pub const ANYWHERE: Self = Self::Anywhere { align: PAGE_SIZE };
}

/// `VmAt`'s two kinds as the syscall's fourth argument.
pub const VM_AT_ANYWHERE: u64 = 0;
pub const VM_AT_FIXED: u64 = 1;

/// Result of [`pipe`]: the read and write ends.
#[derive(Debug, Clone, Copy)]
pub struct PipeEnds {
    pub read: RawHandle,
    pub write: RawHandle,
}

/// File metadata returned by [`fstat`] and [`stat`].
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Stat {
    pub file_type: FileType,
    pub size: u64,
    /// When the file was last written: nanoseconds since the Unix epoch, UTC,
    /// off the wall clock at the write, to the resolution its mount keeps —
    /// the nanosecond on `/tmp` and DATA, two seconds at the flush on FAT. 0 is
    /// undated: written on a machine whose RTC never answered, or shipped in
    /// ROOT's reproducible image.
    pub mtime: u64,
    /// The volume the file is on; 0 for an object that is no file of any
    /// volume. A kernel mount's is below 2^63, a file server's has bit 63 set
    /// (`toyos::fs::SERVED_VOLUME`), so the two minters cannot collide.
    pub volume: u64,
    /// The file on that volume: the same through every path and handle that
    /// reaches it, for as long as it exists and its volume stays mounted, and
    /// no other file's while both exist. 0 exactly when `volume` is.
    pub node: u64,
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
fn syscall(num: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> u64 {
    let ret: u64;
    // SAFETY: a plain register-to-register `syscall` instruction — every
    // input is a `u64` value, not a pointer this function dereferences, so
    // its soundness rests on the callee (the kernel's syscall dispatcher)
    // and on every typed wrapper in this file passing `num`/`a1..a4` that
    // match the syscall it names, not on anything checkable here.
    unsafe {
        core::arch::asm!(
            "syscall",
            in("rdi") num,
            in("rsi") a1,
            in("rdx") a2,
            in("r8") a3,
            in("r9") a4,
            lateout("rax") ret,
            out("rcx") _,
            out("r11") _,
        );
    }
    ret
}

#[cfg(target_arch = "aarch64")]
#[inline(always)]
fn syscall(num: u64, a1: u64, a2: u64, a3: u64, a4: u64) -> u64 {
    let ret: u64;
    // SAFETY: see the x86_64 `syscall` above — same shape, `svc #0` in place
    // of `syscall`, same reasoning.
    unsafe {
        core::arch::asm!(
            "svc #0",
            in("x0") num,
            in("x1") a1,
            in("x2") a2,
            in("x3") a3,
            in("x4") a4,
            lateout("x0") ret,
        );
    }
    ret
}

/// Encode an optional timeout for the kernel ABI.
/// `None` = wait forever (u64::MAX), `Some(n)` = timeout after `n` nanoseconds.
fn encode_timeout(timeout: Option<u64>) -> u64 {
    timeout.unwrap_or(u64::MAX)
}

/// Write bytes to a handle. Returns number of bytes written; a pipe or
/// connection whose other end has closed is [`SyscallError::Gone`]. A pipe
/// write of at most [`MAX_WHOLE_PIPE_WRITE`] bytes writes all of them.
pub fn write(handle: RawHandle, buf: &[u8]) -> Result<usize, SyscallError> {
    check(syscall(SYS_WRITE, handle.0 as u64, buf.as_ptr() as u64, buf.len() as u64, 0)).map(|n| n as usize)
}

/// Read bytes from a handle. Returns number of bytes read.
pub fn read(handle: RawHandle, buf: &mut [u8]) -> Result<usize, SyscallError> {
    check(syscall(SYS_READ, handle.0 as u64, buf.as_mut_ptr() as u64, buf.len() as u64, 0)).map(|n| n as usize)
}

/// Read a file at `offset`: how many bytes, 0 at or past its end. Any handle
/// but a file's is [`SyscallError::NotSupported`].
pub fn pread(handle: RawHandle, buf: &mut [u8], offset: u64) -> Result<usize, SyscallError> {
    check(syscall(SYS_PREAD, handle.0 as u64, buf.as_mut_ptr() as u64, buf.len() as u64, offset)).map(|n| n as usize)
}

/// Write a file at `offset`: how many bytes. Any handle but a file's is
/// [`SyscallError::NotSupported`].
pub fn pwrite(handle: RawHandle, buf: &[u8], offset: u64) -> Result<usize, SyscallError> {
    check(syscall(SYS_PWRITE, handle.0 as u64, buf.as_ptr() as u64, buf.len() as u64, offset)).map(|n| n as usize)
}

/// Exit the current thread only. Does not return.
/// Use `exit()` to exit the entire process (all threads).
pub fn thread_exit(code: i32) -> ! {
    loop { syscall(SYS_THREAD_EXIT, code as u64, 0, 0, 0); }
}

/// Exit the entire process (all threads) with `code`. Does not return.
pub fn exit(code: i32) -> ! {
    loop { syscall(SYS_EXIT, code as u64, 0, 0, 0); }
}

/// Ask the kernel to do one of the things [`SYS_DEBUG`] does. The actions are
/// the `DEBUG_*` constants below, not bare numbers.
pub fn debug(action: u64) -> u64 {
    syscall(SYS_DEBUG, action, 0, 0, 0)
}

/// [`debug`] for the actions that take an argument.
pub fn debug_with(action: u64, arg: u64) -> u64 {
    syscall(SYS_DEBUG, action, arg, 0, 0)
}

/// What [`SYS_DEBUG`] can be asked to do.
///
/// **One declaration, because a number that means something is a constant.** An
/// action's *reason* stays at the kernel arm, where the code it runs is; its
/// number lives here once, where both sides read it.
///
/// **Every action here needs a kernel built with `test-actuators`, and a kernel
/// without it has no `SYS_DEBUG` at all** — the number falls to the dispatch's
/// default and answers `InvalidArgument`, which is what an unassigned number
/// answers. So a constant below is a number a shipping kernel does not have,
/// and the caller of one is a test that has to be given the kernel that does.
pub mod debug_action {
    /// Panic the kernel. Kills the caller's machine, deliberately.
    pub const PANIC: u64 = 0;
    /// Read, in Ring 0, the address the argument names.
    pub const NULL_READ: u64 = 1;
    /// Hold a kernel lock across a scheduler entry.
    pub const LOCK_ACROSS_SWITCH: u64 = 2;
    /// Halt every CPU.
    pub const FATAL_HALT: u64 = 3;
    /// Provoke a real #DF, to exercise the IST1 stack.
    pub const DOUBLE_FAULT: u64 = 4;
    /// Heap allocations either side of `MAX_HEAP_ALLOC`.
    pub const HEAP_AT_CEILING: u64 = 5;
    pub const HEAP_OVER_CEILING: u64 = 6;
    pub const HEAP_AT_CEILING_PAGE_ALIGNED: u64 = 7;
    /// Read the guard page below this CPU's idle stack.
    pub const IDLE_GUARD_READ: u64 = 9;
    /// The kernel canary's address, and whether it still holds what the kernel
    /// wrote.
    pub const CANARY_ADDR: u64 = 10;
    pub const CANARY_CHANGED: u64 = 11;
    /// Hold each other CPU's acknowledgement back for `arg` nanoseconds in
    /// turn, one at a time, and answer the smallest wait any of them cost the
    /// initiator — `0` on a machine with no other CPU to hold back. The
    /// arming is then left standing against every other CPU until
    /// `TLB_ACK_DELAY_DISARM` or the end of a fresh two-second window,
    /// whichever comes first.
    pub const TLB_ACK_DELAY_ARM: u64 = 12;
    pub const TLB_ACK_DELAY_DISARM: u64 = 13;
    /// How many changes to the caller's own address space have reached
    /// another CPU by interrupt: what a change no other CPU runs the space on
    /// must leave as it was.
    pub const SPACE_REACHED: u64 = 29;
    /// How many kernel objects of one kind are alive right now. The argument is
    /// an [`ObjectKind`](crate::handle::ObjectKind) word.
    ///
    /// **Per kind and not a total**: an object of one kind that is never
    /// released is invisible behind ordinary churn in another.
    pub const CENSUS_KIND: u64 = 16;
    /// Put a count this guest can reach in `MAX_SYSINFO_THREADS`'s place, for
    /// the rest of the boot. `SYS_SYSINFO`'s real bound is a thread count no
    /// guest can make, so only the number can move and moving it runs the
    /// shipped count, comparison and refusal.
    pub const LOWER_SYSINFO_BOUND: u64 = 19;
    /// Put one of the caller's own **free** handle slots at the last generation
    /// it can be issued at, and answer the `RawHandle` its next install will
    /// carry. The argument is a slot number; a slot out of range, one that still
    /// holds a handle, and one that has already retired all answer
    /// `InvalidArgument`.
    ///
    /// A slot has 1,048,575 lifecycles, so what a table does at the end of them
    /// is reachable for real only by a test spending two syscalls on each one.
    /// This stages the last lifecycle and nothing else: the generation is the
    /// shipped field, and the install and the close that follow are the shipped
    /// paths making the shipped decision (`kernel::object::handle`).
    pub const SLOT_TO_LAST_GENERATION: u64 = 20;
    /// Emit one patterned kernel log record, `logstorm t=0 i=<arg> …`, whose
    /// text the reader regenerates from its two numbers.
    pub const LOG_PATTERNED: u64 = 21;
    /// Mark the caller's next spawn that reaches its commit: the kernel kills
    /// the process it is placed under after the commit and before the child
    /// lands. That window is the loader's own, so no caller can order a kill
    /// inside it; the kill and the landing that follow are the shipped paths.
    pub const KILL_PLACE_AS_SPAWN_LANDS: u64 = 22;
    /// Mark the caller's next spawn whose child lands: its thread waits there,
    /// before the spawn answers, until the child's exit is published. A child
    /// ending inside the spawn that started it is a race no caller can order;
    /// the landing and the exit either side of the wait are the shipped paths.
    pub const HOLD_SPAWN_UNTIL_CHILD_ENDS: u64 = 23;
    /// The CPU the argument names answers no [`SYS_COUNTERS`](super::SYS_COUNTERS)
    /// round until [`COUNTERS_HEAR`]: a reader finds it as it finds a CPU
    /// with its interrupts closed past the round's bound. A CPU out of range
    /// answers `InvalidArgument`.
    pub const COUNTERS_DEAF: u64 = 24;
    /// Every CPU answers rounds again.
    pub const COUNTERS_HEAR: u64 = 25;
    /// Write the argument's count of [`Kind::Mark`](crate::trace::Kind::Mark)
    /// records into the calling CPU's diary ring back to back, the `i`th with
    /// `data` `i`, and answer the counter ticks the writes took with
    /// interrupts closed. A count past [`TRACE_FLOOD_MOST`] answers
    /// `InvalidArgument`.
    pub const TRACE_FLOOD: u64 = 14;
    /// The most records one [`TRACE_FLOOD`] writes.
    pub const TRACE_FLOOD_MOST: u64 = 1 << 24;
    /// Arm this CPU's timer to fire within 100 µs and wait inside the syscall
    /// for the kernel's own fire. Answers [`RING0_FIRE_REARMED`] once it fired
    /// and re-armed a quantum, [`RING0_FIRE_NEVER`] if none came within
    /// 100 ms, and [`RING0_FIRE_OTHER_SPAN`] if it re-armed something else.
    pub const RING0_TIMER_IN_SYSCALL: u64 = 26;
    /// Play the firmware's side of the Global Lock (ACPI 6.5 §5.2.10.1) on the
    /// FACS's lock word: [`FIRMWARE_OWNS`] takes it, as an SMI handler would,
    /// [`FIRMWARE_ASKS`] sets the pending bit under an owner, as a handler that
    /// found it taken would, and [`FIRMWARE_FREES`] clears both. Answers the
    /// word as it was, or `NotSupported` on a machine with no FACS. No guest's
    /// firmware touches the lock on demand; the take that finds it owned and
    /// the release that owes a signal are the shipped paths.
    pub const ACPI_FIRMWARE_LOCK: u64 = 27;
    pub const FIRMWARE_FREES: u64 = 0;
    pub const FIRMWARE_OWNS: u64 = 1;
    pub const FIRMWARE_ASKS: u64 = 2;
    pub const RING0_FIRE_REARMED: u64 = 0;
    pub const RING0_FIRE_NEVER: u64 = 1;
    pub const RING0_FIRE_OTHER_SPAN: u64 = 2;
    /// Map a global 2 MiB kernel leaf over one frame and read through it, point
    /// the leaf at a second frame and read again with nothing invalidated, the
    /// control, invalidate as the argument says, and read a third time, all on
    /// one CPU with interrupts closed. The argument is
    /// [`GLOBAL_LEAF_DISCHARGED`], the write's own discharge,
    /// [`GLOBAL_LEAF_KERNEL_SHOT_DOWN`], the machine-wide shootdown a
    /// kernel-half change asks, or [`GLOBAL_LEAF_USER_SHOT_DOWN`], the one a
    /// user-half change asks. Answers [`GLOBAL_LEAF_SAW_OLD`] or
    /// [`GLOBAL_LEAF_SAW_NEW`], the frame the third read saw;
    /// [`GLOBAL_LEAF_UNHELD`] where the control already saw the second frame;
    /// [`GLOBAL_LEAF_WIDENED`] where the user-half shootdown's answer on this
    /// CPU covered another CPU's kernel-half one and so dropped global
    /// translations; `NotSupported` on AArch64, `AlreadyExists` where
    /// something is already mapped where the leaf goes.
    pub const GLOBAL_LEAF_REWRITE: u64 = 28;
    pub const GLOBAL_LEAF_DISCHARGED: u64 = 0;
    pub const GLOBAL_LEAF_KERNEL_SHOT_DOWN: u64 = 1;
    pub const GLOBAL_LEAF_USER_SHOT_DOWN: u64 = 2;
    pub const GLOBAL_LEAF_SAW_OLD: u64 = 0;
    pub const GLOBAL_LEAF_SAW_NEW: u64 = 1;
    pub const GLOBAL_LEAF_UNHELD: u64 = 2;
    pub const GLOBAL_LEAF_WIDENED: u64 = 3;
}

/// Create a pipe. Returns the read and write ends.
///
/// **Fallible, because `sys_pipe` is.** It answers `ResourceExhausted` where
/// the pipe's heap cannot be charged or either handle install hits the table
/// cap, and a wrapper that cannot say so hands the caller a pair of handles that are an
/// error word cut in half.
///
/// A packed pair can never be mistaken for an error word: no handle is ever
/// `0xFFFF_FFFF`, because no slot is ever issued at `MAX_GENERATION` — one
/// retires rather than stepping to it — and `SyscallError` occupies only the
/// top 256 values.
pub fn pipe() -> Result<PipeEnds, SyscallError> {
    let raw = check(syscall(SYS_PIPE, 0, 0, 0, 0))?;
    Ok(PipeEnds {
        read: RawHandle((raw >> 32) as u32),
        write: RawHandle((raw & 0xFFFF_FFFF) as u32),
    })
}

/// An event's two sides: [`signal`](Self::signal) carries `WRITE|DUP|TRANSFER`,
/// [`wait`](Self::wait) `READ|WAIT|DUP|TRANSFER`.
pub struct EventEnds {
    pub signal: RawHandle,
    pub wait: RawHandle,
}

/// A fresh event: one coalescing pending signal, raised by its signal side
/// and taken by its wait side. Packed as [`pipe`] is.
pub fn event_create() -> Result<EventEnds, SyscallError> {
    let raw = check(syscall(SYS_EVENT_CREATE, 0, 0, 0, 0))?;
    Ok(EventEnds { signal: RawHandle((raw >> 32) as u32), wait: RawHandle((raw & 0xFFFF_FFFF) as u32) })
}

/// Raise the event; signals before a take are one. Never blocks and
/// allocates nothing. `Gone` once the wait side's last handle has gone.
pub fn event_signal(signal: RawHandle) -> Result<(), SyscallError> {
    check(syscall(SYS_EVENT_SIGNAL, signal.0 as u64, 0, 0, 0)).map(drop)
}

/// Take the pending signal. `WouldBlock` with none pending; `Gone` with none
/// pending and the signal side's last handle gone, which is never a signal.
/// A wait side is readable while either holds, and a readable watch takes
/// nothing.
pub fn event_take(wait: RawHandle) -> Result<(), SyscallError> {
    check(syscall(SYS_EVENT_TAKE, wait.0 as u64, 0, 0, 0)).map(drop)
}

/// Park until `handle` answers the one condition `interest` names
/// ([`READABLE`](crate::inbox::READABLE), [`WRITABLE`](crate::inbox::WRITABLE)
/// or [`OTHER_END_GONE`](crate::inbox::OTHER_END_GONE)), as an inbox watch
/// would, or `timeout_nanos` passes; `u64::MAX` waits for good. Answers the
/// condition's bit, or `0` at the timeout. Needs `WAIT`; an object with no
/// such condition, or a device claim, is `NotSupported`.
pub fn handle_wait(handle: RawHandle, interest: u32, timeout_nanos: u64) -> Result<u32, SyscallError> {
    check(syscall(SYS_HANDLE_WAIT, handle.0 as u64, interest as u64, timeout_nanos, 0)).map(|bit| bit as u32)
}

/// Read the inherited environment variables into `buf`.
/// Returns the number of bytes written, or the required size if buf is too small.
pub fn get_env(buf: &mut [u8]) -> usize {
    syscall(SYS_GET_ENV, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0) as usize
}

/// Spawn a new process.
///
/// Answers a `Process` handle carrying `WAIT|MANAGE|READ|DUP|TRANSFER`. A
/// caller that wants nothing to do with the child closes it; a caller that
/// wants to hand it on transfers it. There is no pid-addressed way back to a
/// process, so this handle is the whole of what a spawn confers. Its slot is
/// taken before an endowment moves: a caller whose table has none is refused
/// `ResourceExhausted` with its table as it was, whatever its endowments
/// would have freed.
///
/// # Safety
/// The raw pointer fields in `SpawnArgs` must point to valid memory, and every
/// handle its endowment names is the caller's to give away: a spawn moves them
/// ([`crate::handle`]).
pub unsafe fn spawn(args: &SpawnArgs) -> Result<RawHandle, SyscallError> {
    check(syscall(SYS_SPAWN, args as *const SpawnArgs as u64, 0, 0, 0))
        .map(|h| RawHandle(h as u32))
}

/// Read this process's endowment table into `buf`: an `[EndowEntry]` count and
/// entries followed by the label blob. Returns the bytes written, or — when
/// `buf` is empty — the bytes the answer needs.
///
/// The one place a name is resolved to a handle at all: there is no global
/// registry, so a process learns what it holds only from its own table.
pub fn endowments(buf: &mut [u8]) -> usize {
    syscall(SYS_ENDOWMENTS, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0) as usize
}

/// Block until the process `proc` names has ended, and take how.
pub fn process_wait(proc: RawHandle) -> Result<End, SyscallError> {
    check(syscall(SYS_PROCESS_WAIT, proc.0 as u64, 0, 0, 0)).map(end_of)
}

/// How the process ended if it has, `Err(WouldBlock)` if it has not.
///
/// [`WNOHANG`] rather than a syscall of its own: this is the same question with
/// the wait taken out, and a caller that polls is asking about the same object.
pub fn process_wait_nonblock(proc: RawHandle) -> Result<End, SyscallError> {
    check(syscall(SYS_PROCESS_WAIT, proc.0 as u64, WNOHANG, 0, 0)).map(end_of)
}

/// The end the kernel answered, which is one it defines or a kernel bug.
fn end_of(word: u64) -> End {
    End::from_word(word).unwrap_or_else(|| panic!("SYS_PROCESS_WAIT answered {word:#x}, which names no end"))
}

/// Kill the process `proc` names. Answers `Ok` for one already dead: the
/// caller asked for it to be gone and it is.
pub fn process_kill(proc: RawHandle) -> Result<(), SyscallError> {
    check_unit(syscall(SYS_PROCESS_KILL, proc.0 as u64, 0, 0, 0))
}

/// Ask the process `proc` names, and every process below it, to quit for
/// `reason`. A process that listens to its notice finds the reason in it; one
/// that never listened, or has not yet read the reason it was last asked, is
/// killed. Answers `Ok` for one already ended, as [`process_kill`] does.
pub fn process_quit(proc: RawHandle, reason: crate::quit::Reason) -> Result<(), SyscallError> {
    check_unit(syscall(SYS_PROCESS_QUIT, proc.0 as u64, u64::from(reason.to_byte()), 0, 0))
}

/// Copy records into `out`, oldest first and merged by `at_ns`, advancing
/// `cursor`. Answers how many records were written, and `0` when there is
/// nothing new.
///
/// **It never blocks.** A caller with nothing to read arms on a readiness
/// source and parks; a syscall that waited would be a second blocking mechanism
/// in a kernel that is converging on one.
///
/// `cursor` is a [`crate::log::CursorHead`] and its positions; this stores
/// how many positions follow into the head. `out` is whole
/// [`crate::log::LogRecord`]s at a fixed stride, so the caller indexes by shift
/// and the kernel does no length arithmetic. A buffer of no records, a cursor
/// with other than a position per shard, and a position ahead of its shard are
/// `InvalidArgument` — untrusted input that cannot be satisfied is refused,
/// never truncated to fit — and a refusal past the head still answers the
/// shard count in it.
pub fn log_read(
    syscap: RawHandle,
    cursor: &mut [u64],
    out: &mut [crate::log::LogRecord],
) -> Result<usize, SyscallError> {
    let cursor = sized_cursor(cursor)?;
    check(syscall(SYS_LOG_READ, syscap.0 as u64, cursor as u64, out.as_mut_ptr() as u64, out.len() as u64))
        .map(|n| n as usize)
}

/// `cursor`'s head told how many positions follow it, as the address a read takes.
fn sized_cursor(cursor: &mut [u64]) -> Result<*mut u64, SyscallError> {
    use crate::log::{CursorHead, CURSOR_HEAD_WORDS};
    let (head, positions) = cursor.split_first_chunk_mut::<CURSOR_HEAD_WORDS>().ok_or(SyscallError::InvalidArgument)?;
    let slots = u32::try_from(positions.len()).map_err(|_| SyscallError::InvalidArgument)?;
    *head = CursorHead { slots, ..CursorHead::from_words(*head) }.to_words();
    Ok(cursor.as_mut_ptr())
}

/// Mark this handle as the controlling TTY for this process.
pub fn mark_tty(handle: RawHandle) {
    syscall(SYS_MARK_TTY, handle.0 as u64, 0, 0, 0);
}

/// Threads one process holds, its main thread and every exited one not yet
/// joined among them; [`thread_spawn`] past it is `ResourceExhausted`.
pub const MAX_THREADS: usize = 4096;

/// Spawn a new thread with the given entry point, stack pointer, argument, and stack base.
/// `stack_base` is the bottom of the user stack (for stack info queries). An
/// `entry` outside the user half is `InvalidArgument`.
///
/// # Safety
/// `entry` must be a valid function pointer and `stack`/`stack_base` must
/// describe a valid, correctly-sized stack region.
pub unsafe fn thread_spawn(entry: u64, stack: u64, arg: u64, stack_base: u64) -> u64 {
    syscall(SYS_THREAD_SPAWN, entry, stack, arg, stack_base)
}

/// Wait for thread `tid` to exit.
pub fn thread_join(tid: u64) -> u64 {
    syscall(SYS_THREAD_JOIN, tid, 0, 0, 0)
}

/// Words in a CPU mask: word `i` bit `b` is CPU `64 * i + b`, by the dense
/// index [`current_cpu`] reports and [`cpu_count`] counts. 1024 CPUs, as
/// Linux's `CPU_SETSIZE`.
pub const CPU_MASK_WORDS: usize = 16;

/// Let thread `tid` of the calling process run only on the CPUs `mask` names.
/// `tid` names a thread as [`thread_join`]'s does, 0 the main thread; a
/// thread names itself by [`current_tid`](crate::current_tid). A shorter
/// mask's missing words are empty.
///
/// The mask is a placement choice, not a right: any non-empty set of online
/// CPUs is accepted, wider or narrower than the last. A new thread or process
/// starts with its spawning thread's mask.
///
/// `NotFound` for a `tid` that is not a live thread of this process;
/// `InvalidArgument` for an empty mask, one naming a CPU at or past
/// [`cpu_count`], or one longer than [`CPU_MASK_WORDS`].
pub fn thread_set_affinity(tid: u64, mask: &[u64]) -> Result<(), SyscallError> {
    check(syscall(SYS_THREAD_SET_AFFINITY, tid, mask.as_ptr() as u64, mask.len() as u64, 0)).map(|_| ())
}

/// The online CPUs thread `tid` of the calling process may run on, `tid` as
/// [`thread_set_affinity`] names it. `NotFound` for a `tid` that is not a
/// live thread of this process.
pub fn thread_affinity(tid: u64) -> Result<[u64; CPU_MASK_WORDS], SyscallError> {
    let mut mask = [0u64; CPU_MASK_WORDS];
    check(syscall(SYS_THREAD_GET_AFFINITY, tid, mask.as_mut_ptr() as u64, CPU_MASK_WORDS as u64, 0))?;
    Ok(mask)
}

/// The CPU the caller was running on during the call, by the index a mask
/// names. It may have moved by the time this returns.
pub fn current_cpu() -> u32 {
    syscall(SYS_CURRENT_CPU, 0, 0, 0, 0) as u32
}

/// Set the name of the calling thread (up to 28 bytes, truncated).
pub fn set_thread_name(name: &[u8]) {
    syscall(SYS_SET_THREAD_NAME, name.as_ptr() as u64, name.len() as u64, 0, 0);
}

/// The most links `SYS_OPEN` follows from the name it is given: the link a
/// chain reaches past them is refused `InvalidArgument`.
pub const MAX_LINKS: u32 = 10;

/// The longest path a file server's wire carries (`sdk/toyos/src/fs.rs`), and
/// libc's `PATH_MAX` (`sdk/libc/include/limits.h`).
pub const MAX_PATH: usize = 4096;

/// The longest pipe write that lands whole: never interleaved with another
/// writer's bytes, it waits for room for all of it, and a pipe end polls
/// writable only with this much room. A longer write may land in parts.
/// libc's `PIPE_BUF` (`sdk/libc/include/limits.h`).
pub const MAX_WHOLE_PIPE_WRITE: usize = 4096;

/// The most a pipe holds unread: a write past it waits for its reader. The
/// kernel takes memory for what a pipe holds as it fills and gives it back as
/// it drains, so this is a bound, not what a pipe costs. A pipe that has held
/// a byte keeps what an empty one needs: a write of at most
/// [`MAX_WHOLE_PIPE_WRITE`] bytes into it while it is empty is never refused
/// for memory.
pub const PIPE_CAPACITY: usize = 2 << 20;

/// Open a file.
pub fn open(path: &[u8], flags: OpenFlags) -> Result<RawHandle, SyscallError> {
    check(syscall(SYS_OPEN, path.as_ptr() as u64, path.len() as u64, flags.0, 0)).map(|v| RawHandle(v as u32))
}

/// Close a handle.
///
/// # Safety
/// `handle` is this process's and nothing else answers for it: no owner
/// closes or uses it again ([`crate::handle`]).
pub unsafe fn close(handle: RawHandle) {
    syscall(SYS_CLOSE, handle.0 as u64, 0, 0, 0);
}

/// Seek within a file handle. Returns new offset.
pub fn seek(handle: RawHandle, pos: SeekFrom) -> Result<u64, SyscallError> {
    let (offset, whence) = match pos {
        SeekFrom::Start(n) => (n as i64, 0u64),
        SeekFrom::Current(n) => (n, 1u64),
        SeekFrom::End(n) => (n, 2u64),
    };
    check(syscall(SYS_SEEK, handle.0 as u64, offset as u64, whence, 0))
}

/// Get file metadata for a file handle.
pub fn fstat(handle: RawHandle) -> Result<Stat, SyscallError> {
    let mut stat = Stat::default();
    check_unit(syscall(SYS_FSTAT, handle.0 as u64, &mut stat as *mut Stat as u64, 0, 0))?;
    Ok(stat)
}

/// What the kernel's `path` names: a file, a directory, or, unless `follow`
/// follows it, a symlink, each with the identity [`fstat`] answers a file.
pub fn stat(path: &[u8], follow: bool) -> Result<Stat, SyscallError> {
    let mut stat = Stat::default();
    let out = &mut stat as *mut Stat as u64;
    check_unit(syscall(SYS_STAT, path.as_ptr() as u64, path.len() as u64, out, follow as u64))?;
    Ok(stat)
}

/// Flush a file handle to disk, or a partition claim's writes out of its
/// device's cache ([`crate::part`]): `Ok` is the device's own word that every
/// write the claim returned from before this call is durable.
pub fn fsync(handle: RawHandle) -> Result<(), SyscallError> {
    check_unit(syscall(SYS_FSYNC, handle.0 as u64, 0, 0, 0))
}

/// Read directory entries. Returns the number of bytes the listing *needs*.
///
/// `Ok(n)` with `n <= buf.len()` means the entries are in `buf`; `n >
/// buf.len()` means nothing was written and `n` is the size to retry with.
/// The kernel never writes a partial listing — see `sys_readdir`.
///
/// The error is returned rather than folded into `0`: "the directory is too
/// large to list" and "the directory is empty" are different answers and a
/// caller has to be able to tell them apart.
pub fn readdir(path: &[u8], buf: &mut [u8]) -> Result<usize, SyscallError> {
    let n = syscall(SYS_READDIR, path.as_ptr() as u64, path.len() as u64, buf.as_mut_ptr() as u64, buf.len() as u64);
    match SyscallError::from_u64(n) {
        Some(e) => Err(e),
        None => Ok(n as usize),
    }
}

/// The kind byte a [`readdir`] entry opens with: a file.
pub const DIRENT_FILE: u8 = 1;
/// The kind byte a [`readdir`] entry opens with: a directory.
pub const DIRENT_DIR: u8 = 2;

/// One entry of a [`readdir`] listing: its kind byte, its name, a NUL, and its
/// node as eight bytes, little-endian: the [`Stat::node`] [`stat`] answers it.
pub struct Dirent<'a> {
    pub is_dir: bool,
    pub name: &'a [u8],
    pub node: u64,
}

/// The entry at `*at` in a whole [`readdir`] listing, with `*at` moved past
/// it; `None` once `*at` is at its end. A listing of any other shape is a
/// kernel that broke this ABI, and panics.
pub fn dirent<'a>(listing: &'a [u8], at: &mut usize) -> Option<Dirent<'a>> {
    let rest = listing.get(*at..).filter(|r| !r.is_empty())?;
    let is_dir = match rest[0] {
        DIRENT_FILE => false,
        DIRENT_DIR => true,
        kind => panic!("SYS_READDIR answered an entry of kind {kind}"),
    };
    let len = rest[1..].iter().position(|&b| b == 0).expect("SYS_READDIR answered a name with no NUL");
    let node = rest
        .get(len + 2..len + 10)
        .expect("SYS_READDIR answered an entry with no node");
    *at += len + 10;
    Some(Dirent { is_dir, name: &rest[1..1 + len], node: u64::from_le_bytes(node.try_into().unwrap()) })
}

/// Delete a file or directory.
pub fn delete(path: &[u8]) -> Result<(), SyscallError> {
    check_unit(syscall(SYS_DELETE, path.as_ptr() as u64, path.len() as u64, 0, 0))
}

/// Change current working directory.
pub fn chdir(path: &[u8]) -> Result<(), SyscallError> {
    check_unit(syscall(SYS_CHDIR, path.as_ptr() as u64, path.len() as u64, 0, 0))
}

/// Get the current working directory.
///
/// Returns the length the path *needs*, not the number of bytes written:
/// `n <= buf.len()` means the path is in `buf[..n]`, and `n > buf.len()` means
/// nothing was written and `n` is the size to allocate before retrying. Pass an
/// empty buffer to ask the length alone. `0` is the error return.
///
/// The required length rather than a truncated count, because a count cannot
/// distinguish an exact fit from a silent truncation — and a caller with a fixed
/// buffer would take a valid-looking path to the wrong directory.
pub fn getcwd(buf: &mut [u8]) -> usize {
    let n = syscall(SYS_GETCWD, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0);
    if SyscallError::from_u64(n).is_some() { 0 } else { n as usize }
}

/// Fill `buf` with cryptographically secure random bytes.
///
/// `Err` is a source that had nothing to give, and `buf` then holds no
/// randomness a caller may use: every caller decides what that failure is,
/// because a value drawn from a refused source is a predictable one.
pub fn random(buf: &mut [u8]) -> Result<(), SyscallError> {
    check_unit(syscall(SYS_RANDOM, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0))
}

/// Seconds since the Unix epoch (1970-01-01 00:00:00 UTC).
///
/// `None` is a machine that never said what time it is — an RTC that is absent,
/// wedged, or answering with something that is not a date. It is `None` for the
/// whole of such a boot rather than intermittently, because the kernel reads
/// the clock once.
/// Cheap: the kernel serves it from an anchor it took at boot plus the
/// monotonic clock, so this is a syscall and not a device access.
pub fn clock_epoch() -> Option<u64> {
    check(syscall(SYS_CLOCK_EPOCH, 0, 0, 0, 0)).ok()
}

/// Power the machine off, presenting a `SysCap` that carries
/// [`Rights::POWER`](crate::handle::Rights::POWER).
///
/// **The only way this comes back is refused.** A shutdown that happened has no
/// caller left to answer, so the return type is the refusal alone; the loop is
/// what says so, asking again rather than reporting a success that did not happen.
/// [`SyscallError::NotSupported`] is a machine whose `acpi` claim no holder
/// has supplied the power-off's sleep type on ([`acpi_s5`]): nothing was
/// stopped.
pub fn shutdown(syscap: RawHandle) -> SyscallError {
    loop {
        if let Err(e) = check_unit(syscall(SYS_SHUTDOWN, syscap.0 as u64, 0, 0, 0)) {
            return e;
        }
    }
}

/// [`shutdown`]'s sibling on the same right, refused the same way and looping for the same reason.
pub fn reboot(syscap: RawHandle) -> SyscallError {
    loop {
        if let Err(e) = check_unit(syscall(SYS_REBOOT, syscap.0 as u64, 0, 0, 0)) {
            return e;
        }
    }
}

/// The clock now as a log line's time, nanoseconds since the counter's zero,
/// read by the kernel: what [`crate::clock::stamp_ns`] answers where the page
/// is demoted. Needs no handle; it fails for nobody.
pub fn clock_now() -> u64 {
    syscall(SYS_CLOCK_NOW, 0, 0, 0, 0)
}

/// Which stop [`machine_stops`] asks about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
pub enum MachineStop {
    Shutdown = 0,
    Reboot = 1,
}

impl MachineStop {
    pub fn from_u64(word: u64) -> Option<Self> {
        match word {
            0 => Some(Self::Shutdown),
            1 => Some(Self::Reboot),
            _ => None,
        }
    }
}

/// `Ok` where [`shutdown`] or [`reboot`], as `stop` names, would stop this
/// machine; the refusal it would answer before stopping anything where not,
/// [`SyscallError::NotSupported`] for a machine without it. Lets a caller
/// refuse a stop before it ends what the stop would have ended.
pub fn machine_stops(syscap: RawHandle, stop: MachineStop) -> Result<(), SyscallError> {
    check_unit(syscall(SYS_MACHINE_STOPS, syscap.0 as u64, stop as u64, 0, 0))
}

/// The device classes, their wire numbers, and the name a `system.toml`
/// `devices` entry and a `dev:` endowment label spell each one with.
///
/// **One row per class, so the four cannot disagree.** The build system checks
/// a config against this table, the supervisor mints from it, and a claimant finds
/// its own claim by it; a second spelling anywhere is a class a config can name
/// and no program can find. The wire number is here too, because a class whose
/// number and name came from different lists is the same defect one level down.
macro_rules! device_classes {
    ($($(#[$meta:meta])* $variant:ident = $num:literal => $name:literal),+ $(,)?) => {
        #[repr(u64)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum DeviceType {
            $($(#[$meta])* $variant = $num),+
        }

        impl DeviceType {
            /// What a manifest calls this class.
            pub fn class_name(self) -> &'static str {
                match self { $(Self::$variant => $name),+ }
            }

            /// The class a config named, or `None` — a typo in a `devices`
            /// list, refused where the image is built.
            pub fn from_class_name(name: &str) -> Option<Self> {
                match name { $($name => Some(Self::$variant),)+ _ => None }
            }

            /// The wire number a syscall carries, decoded once.
            pub fn from_raw(raw: u64) -> Option<Self> {
                match raw { $($num => Some(Self::$variant),)+ _ => None }
            }

            /// Every class, for a caller that must consider all of them.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
        }
    };
}

device_classes! {
    // 0 was `keyboard` and 1 `mouse`, the kernel's input streams.
    Framebuffer = 2 => "framebuffer",
    /// One PCI function, driven by whoever holds the claim: the kernel binds no
    /// driver to it, keeps config space, puts it in an address space of its own
    /// before it lets it master the bus, and programs its interrupt vector.
    ///
    /// A claim names which function by its
    /// [`Arrival`](crate::inventory::Arrival), and no name is spelled for it
    /// here: which functions a manifest row asks for is `toyos-manifest`'s
    /// grammar, resolved against the inventory.
    PciFunction = 7 => "pci",
    // 8 was `part`, one GPT partition of a disk this kernel drove.
    /// One legacy ISA function, driven by whoever holds the claim: the process
    /// that first reads the claim may `in` and `out` the function's ports for
    /// the rest of its life and no other process may, and the function's lines
    /// are answered as interrupt records on the claim ([`IsaId`]).
    ///
    /// Like `pci`, a class whose name is not the whole of the entry —
    /// `isa:<port>,…:<irq>,…` — and [`DeviceRequest`] is the one parser.
    Isa = 9 => "isa",
    /// The machine's ACPI fixed hardware, served by whoever holds the claim
    /// ([`crate::acpi`]): the kernel puts the machine in ACPI mode at the
    /// mint, and the ports the firmware's tables name are the holder's.
    Acpi = 10 => "acpi",
}

/// What identifies a PCI *card*, not the slot firmware put it in: the same
/// card is `00:03.0` on one machine and `00:1f.6` on another. Two of one card
/// carry one id, so it names no single function, and a claim names one by its
/// [`Arrival`](crate::inventory::Arrival).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PciId {
    pub vendor: u16,
    pub device: u16,
}

impl PciId {
    /// The id as one word.
    pub const fn wire(self) -> u64 {
        ((self.vendor as u64) << 16) | self.device as u64
    }

    /// [`Self::wire`] decoded; `None` for anything with bits above 32.
    pub const fn from_wire(raw: u64) -> Option<Self> {
        if raw > u32::MAX as u64 {
            return None;
        }
        Some(Self { vendor: (raw >> 16) as u16, device: raw as u16 })
    }
}

/// Exactly four lowercase hex digits and nothing else.
///
/// One spelling per value, because every reader of a name compares the
/// string: `1af4:41` and `1af4:0041`, or `1af4:1041` and `1AF4:1041`, would
/// otherwise be two names for one card.
fn hex16(text: &str) -> Option<u16> {
    if text.len() != 4 {
        return None;
    }
    let mut value = 0u16;
    for byte in text.bytes() {
        let digit = match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            _ => return None,
        };
        value = (value << 4) | digit as u16;
    }
    Some(value)
}

/// A legacy ISA function named by everything it decodes and raises:
/// `isa:0060,0064:1,12` is the i8042's two ports and its keyboard and aux
/// lines.
///
/// **The name is the grant**, so it is exact: the kernel opens these ports and
/// routes these lines, and a set that is not one function it can hand out
/// whole is refused rather than trimmed to one it can. One spelling per set —
/// ports as four lowercase hex digits, lines in decimal with no leading zero,
/// each list strictly ascending — because every reader compares the string.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct IsaId {
    /// Ascending and nonzero, then zeros: port 0 is the DMA controller's and
    /// never a function a process drives, which is what lets 0 mean no port.
    ports: [u16; IsaId::MAX_PORTS],
    /// One bit per ISA line.
    irqs: u16,
}

impl IsaId {
    pub const MAX_PORTS: usize = 4;
    /// Bounded so the longest name fits [`DeviceRequest::MAX_NAME`].
    pub const MAX_IRQS: usize = 4;
    /// The ISA bus has sixteen lines.
    const LINES: u8 = 16;

    /// `None` for a set with no port or no line, a port 0, a line past 15, a
    /// list longer than its bound, or a list not strictly ascending.
    pub fn new(ports: &[u16], irqs: &[u8]) -> Option<Self> {
        if ports.is_empty() || ports.len() > Self::MAX_PORTS || ports[0] == 0 || !ascending(ports) {
            return None;
        }
        if irqs.is_empty() || irqs.len() > Self::MAX_IRQS || !ascending(irqs) {
            return None;
        }
        let mut set = Self { ports: [0; Self::MAX_PORTS], irqs: 0 };
        set.ports[..ports.len()].copy_from_slice(ports);
        for &irq in irqs {
            if irq >= Self::LINES {
                return None;
            }
            set.irqs |= 1 << irq;
        }
        Some(set)
    }

    pub fn ports(&self) -> impl Iterator<Item = u16> + '_ {
        self.ports.iter().copied().take_while(|&port| port != 0)
    }

    pub fn irqs(&self) -> impl Iterator<Item = u8> + '_ {
        (0..Self::LINES).filter(|line| self.irqs & (1 << line) != 0)
    }

    /// `"<port>,…:<irq>,…"`, the tail of an `isa:` entry.
    pub fn parse(text: &str) -> Option<Self> {
        let (ports, irqs) = text.split_once(':')?;
        let mut port_list = [0u16; Self::MAX_PORTS];
        let mut n_ports = 0;
        for port in ports.split(',') {
            *port_list.get_mut(n_ports)? = hex16(port)?;
            n_ports += 1;
        }
        let mut irq_list = [0u8; Self::MAX_IRQS];
        let mut n_irqs = 0;
        for irq in irqs.split(',') {
            *irq_list.get_mut(n_irqs)? = decimal_line(irq)?;
            n_irqs += 1;
        }
        Self::new(&port_list[..n_ports], &irq_list[..n_irqs])
    }

    /// The two selector words [`device_claim`] carries: the ports in 16-bit
    /// lanes from the low end, and the lines as a mask.
    pub fn wire(self) -> [u64; 2] {
        let ports = self
            .ports
            .iter()
            .enumerate()
            .fold(0u64, |word, (lane, &port)| word | (port as u64) << (16 * lane));
        [ports, self.irqs as u64]
    }

    /// The selector words decoded, refusing whatever [`Self::new`] refuses.
    pub fn from_wire([ports, irqs]: [u64; 2]) -> Option<Self> {
        let lanes: [u16; Self::MAX_PORTS] =
            core::array::from_fn(|lane| (ports >> (16 * lane)) as u16);
        let named = lanes.iter().take_while(|&&port| port != 0).count();
        if lanes[named..].iter().any(|&port| port != 0) || irqs > u16::MAX as u64 {
            return None;
        }
        let mut lines = [0u8; Self::LINES as usize];
        let mut n = 0;
        for line in 0..Self::LINES {
            if irqs & (1 << line) != 0 {
                lines[n] = line;
                n += 1;
            }
        }
        if n > Self::MAX_IRQS {
            return None;
        }
        Self::new(&lanes[..named], &lines[..n])
    }
}

fn ascending<T: PartialOrd>(list: &[T]) -> bool {
    list.windows(2).all(|pair| pair[0] < pair[1])
}

/// One ISA line in decimal, `0` to `15`, with no sign and no leading zero.
fn decimal_line(text: &str) -> Option<u8> {
    let bytes = text.as_bytes();
    if bytes.is_empty() || bytes.len() > 2 || (bytes.len() == 2 && bytes[0] == b'0') {
        return None;
    }
    let mut value = 0u8;
    for &byte in bytes {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value * 10 + (byte - b'0');
    }
    (value < IsaId::LINES).then_some(value)
}

/// What a claim asks for: a class, and for [`DeviceType::PciFunction`] which
/// function, and for an ISA function which ports and lines.
///
/// Every request but a PCI function's is also a `devices` entry, spelled once
/// here ([`Self::parse`], [`Self::write_name`]): the manifest that names it and
/// the claimant that looks its claim up by that name read one spelling.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DeviceRequest {
    Class(DeviceType),
    Function(crate::inventory::Arrival),
    Isa(IsaId),
}

impl DeviceRequest {
    /// The spelling that carries an ISA function's ports and lines.
    pub const ISA_PREFIX: &'static str = "isa:";

    /// The longest a `devices` entry, and so a `dev:` label's tail, can be:
    /// an `isa:` entry's whole set, rounded up to a multiple of eight.
    pub const MAX_NAME: usize = 40;

    /// The request a `devices` entry spells, or `None`; never a
    /// [`Self::Function`], which has no spelling.
    pub fn parse(name: &str) -> Option<Self> {
        if let Some(set) = name.strip_prefix(Self::ISA_PREFIX) {
            return IsaId::parse(set).map(Self::Isa);
        }
        let class = DeviceType::from_class_name(name)?;
        match class {
            // A bare `pci` or `isa` names no device, and would otherwise leave
            // the selector at zero.
            DeviceType::PciFunction | DeviceType::Isa => None,
            _ => Some(Self::Class(class)),
        }
    }

    pub const fn class(self) -> DeviceType {
        match self {
            Self::Class(class) => class,
            Self::Function(_) => DeviceType::PciFunction,
            Self::Isa(_) => DeviceType::Isa,
        }
    }

    /// The two selector words [`device_claim`] carries; a class that names at
    /// most one device on the machine carries none.
    pub fn selector(self) -> [u64; 2] {
        match self {
            Self::Class(_) => [0, 0],
            Self::Function(arrival) => [arrival.0 as u64, 0],
            Self::Isa(set) => set.wire(),
        }
    }

    /// This request's `devices` spelling written into `buf`, which
    /// [`Self::MAX_NAME`] bounds, or `None` for a [`Self::Function`], which has
    /// none. The name is also the tail of the `dev:` label the claim arrives
    /// under, and it is composed here so the two cannot drift.
    pub fn write_name(self, buf: &mut [u8; Self::MAX_NAME]) -> Option<&str> {
        Some(match self {
            Self::Function(_) => return None,
            Self::Class(class) => {
                let name = class.class_name();
                buf[..name.len()].copy_from_slice(name.as_bytes());
                core::str::from_utf8(&buf[..name.len()]).expect("a class name is ASCII")
            }
            Self::Isa(set) => {
                let mut at = Self::ISA_PREFIX.len();
                buf[..at].copy_from_slice(Self::ISA_PREFIX.as_bytes());
                for (i, port) in set.ports().enumerate() {
                    if i > 0 {
                        buf[at] = b',';
                        at += 1;
                    }
                    for shift in [12, 8, 4, 0] {
                        buf[at] = HEX[((port >> shift) & 0xF) as usize];
                        at += 1;
                    }
                }
                buf[at] = b':';
                at += 1;
                for (i, line) in set.irqs().enumerate() {
                    if i > 0 {
                        buf[at] = b',';
                        at += 1;
                    }
                    if line >= 10 {
                        buf[at] = b'1';
                        at += 1;
                    }
                    buf[at] = b'0' + line % 10;
                    at += 1;
                }
                core::str::from_utf8(&buf[..at]).expect("hex, digits and separators are ASCII")
            }
        })
    }
}

const HEX: &[u8; 16] = b"0123456789abcdef";

/// Mint a device claim for `request`, presenting a `SysCap` handle that carries
/// [`Rights::DEVICE`]. `NotFound` for a class no driver registered, or a PCI
/// arrival or partition this machine does not have — the supervisor endows what exists
/// and logs what it did not. `AlreadyExists` names one already claimed, and
/// `PermissionDenied` one the kernel drives itself — for a partition, one it
/// has mounted. A partition GUID two partitions carry answers
/// `InvalidArgument`, because an ambiguous name is refused rather than resolved
/// to the first match; so does a nonzero selector word the class does not read.
///
/// The claim comes back **without** [`Rights::DUP`], so it can only be moved,
/// which is what makes endowing one to a child a provable hand-off.
///
/// [`Rights::DEVICE`]: crate::handle::Rights::DEVICE
/// [`Rights::DUP`]: crate::handle::Rights::DUP
pub fn device_claim(syscap: RawHandle, request: DeviceRequest) -> Result<RawHandle, SyscallError> {
    let [lo, hi] = request.selector();
    check(syscall(SYS_DEVICE_CLAIM, syscap.0 as u64, request.class() as u64, lo, hi))
        .map(|v| RawHandle(v as u32))
}

/// Make a new job, presenting a `SysCap` handle that carries
/// [`Rights::JOBS`], and answer a handle to it carrying `WRITE`, `DUP` and
/// `TRANSFER`: what a spawn endows under [`JOB_LABEL`] to start an independent
/// job, with its own share of the CPU.
///
/// [`Rights::JOBS`]: crate::handle::Rights::JOBS
pub fn job_create(syscap: RawHandle) -> Result<RawHandle, SyscallError> {
    check(syscall(SYS_JOB_CREATE, syscap.0 as u64, 0, 0, 0)).map(|v| RawHandle(v as u32))
}

/// Hold `bytes` back for the job `job` names, which only it may then take
/// however short every other job runs, presenting a `SysCap` handle that
/// carries [`Rights::FLOOR`] and the job's handle with `WRITE`. The floor
/// lasts while that job handle's object does, and goes with its last
/// handle. `ResourceExhausted` where what is free now cannot hold it, and
/// `AlreadyExists` where another handle's object already holds one for the
/// job.
///
/// [`Rights::FLOOR`]: crate::handle::Rights::FLOOR
pub fn job_floor(syscap: RawHandle, job: RawHandle, bytes: u64) -> Result<(), SyscallError> {
    check_unit(syscall(SYS_JOB_FLOOR, syscap.0 as u64, job.0 as u64, bytes, 0))
}

/// Hold every idle CPU to waking within `bound_ns` for as long as the
/// returned handle lives: from when this returns no CPU enters a state slower
/// to leave than the least bound held, and each CPU waiting in one has been
/// sent a wake, which pays that state's exit once. Needs
/// [`Rights::WAKE_LATENCY`]; a bound of 0 is refused
/// [`SyscallError::InvalidArgument`].
///
/// [`Rights::WAKE_LATENCY`]: crate::handle::Rights::WAKE_LATENCY
pub fn wake_latency_hold(syscap: RawHandle, bound_ns: u64) -> Result<RawHandle, SyscallError> {
    check(syscall(SYS_WAKE_LATENCY_HOLD, syscap.0 as u64, bound_ns, 0, 0)).map(|raw| RawHandle(raw as u32))
}

/// What the account of the job `job` names holds, its handle carrying
/// `READ`.
pub fn account_info(job: RawHandle, info: &mut AccountInfo) -> Result<(), SyscallError> {
    check_unit(syscall(
        SYS_ACCOUNT_INFO,
        job.0 as u64,
        info as *mut AccountInfo as u64,
        core::mem::size_of::<AccountInfo>() as u64,
        0,
    ))
}

/// A handle to the job the caller runs in, carrying `WRITE`, `DUP` and
/// `TRANSFER`: what a launch carries for its program to join the caller's job.
/// Asks no right: it names only what the caller is already in.
pub fn job_own() -> Result<RawHandle, SyscallError> {
    check(syscall(SYS_JOB_OWN, 0, 0, 0, 0)).map(|v| RawHandle(v as u32))
}

/// Enter the real-time scheduling band, presenting a `SysCap` handle that
/// carries [`Rights::RT`]. The privilege a device claim was never enough to
/// confer.
///
/// [`Rights::RT`]: crate::handle::Rights::RT
pub fn rt_enter(syscap: RawHandle) -> Result<(), SyscallError> {
    check_unit(syscall(SYS_RT_ENTER, syscap.0 as u64, 0, 0, 0))
}

/// How wide a register access is.
///
/// Not a convenience: config space holds 8-, 16- and 32-bit registers, and a
/// wider read than a register names reads its neighbour too.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u64)]
pub enum RegWidth {
    U8 = 1,
    U16 = 2,
    U32 = 4,
}

impl RegWidth {
    pub fn from_raw(raw: u64) -> Option<Self> {
        match raw {
            1 => Some(Self::U8),
            2 => Some(Self::U16),
            4 => Some(Self::U32),
            _ => None,
        }
    }

    pub const fn bytes(self) -> u64 {
        self as u64
    }

    /// The widest value this access can carry. A caller handing a wider one is
    /// naming bits the register does not have.
    pub const fn max_value(self) -> u32 {
        match self {
            Self::U8 => u8::MAX as u32,
            Self::U16 => u16::MAX as u32,
            Self::U32 => u32::MAX,
        }
    }
}

/// Read one register of the config space of the PCI function `handle`
/// claims, at byte `offset`. There is no write: every write config space takes
/// is the kernel's.
pub fn device_reg_read(
    handle: RawHandle,
    offset: u64,
    width: RegWidth,
) -> Result<u32, SyscallError> {
    check(syscall(SYS_DEVICE_REG_READ, handle.0 as u64, offset, width.bytes(), 0))
        .map(|v| v as u32)
}

// Ports and namespaces

/// Both ends of a fresh port.
///
/// Two types and not one object with a direction right: "accept the
/// connections of a service you were only given access to" is a state that
/// cannot be written, the same way a pipe's two ends are two types.
pub struct Port {
    pub acceptor: RawHandle,
    pub connector: RawHandle,
}

/// Make a port. Needs no right and grants none — a port with no clients is not
/// authority.
pub fn port_create() -> Result<Port, SyscallError> {
    let raw = syscall(SYS_PORT_CREATE, 0, 0, 0, 0);
    if let Some(e) = SyscallError::from_u64(raw) {
        return Err(e);
    }
    Ok(Port {
        acceptor: RawHandle((raw >> 32) as u32),
        connector: RawHandle((raw & 0xFFFF_FFFF) as u32),
    })
}

/// One `(name, connector)` pair `SYS_NAMESPACE_BUILD` adds.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NamespaceEntry {
    pub off: u32,
    pub len: u32,
    pub connector: RawHandle,
    /// Named, so nothing leaks kernel stack into it.
    pub _pad: u32,
}

/// One name carried over from the base namespace.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct NameRef {
    pub off: u32,
    pub len: u32,
}

crate::user_safe! {
    /// Arguments for [`SYS_NAMESPACE_BUILD`], passed as a single pointer.
    ///
    /// A namespace is immutable once built: there is no insert, no remove and no
    /// replace, so a narrower one is a *new* object built from this one and a
    /// handle to a namespace is a handle to a fixed set.
    #[derive(Clone, Copy)]
    pub struct NamespaceBuild {
        /// [`HANDLE_INVALID`] for an empty base.
        ///
        /// [`HANDLE_INVALID`]: crate::handle::HANDLE_INVALID
        pub base: RawHandle,
        /// [`NAMESPACE_KEEP_ALL`], and nothing else.
        pub flags: u32,
        /// `[NameRef]` — the names to carry over from `base`.
        pub keep_ptr: u64,
        pub keep_n: u64,
        /// `[NamespaceEntry]` — new bindings.
        pub add_ptr: u64,
        pub add_n: u64,
        /// The blob every `off`/`len` above indexes into.
        pub names_ptr: u64,
        pub names_len: u64,
    }
}

const _: () = assert!(core::mem::size_of::<NamespaceEntry>() == 16);
const _: () = assert!(core::mem::size_of::<NameRef>() == 8);

/// Carry over every name `base` holds — no syscall enumerates a namespace, so
/// no `keep` list can spell it. `keep_n` must be `0` beside it, and a name it
/// carries that `add` also binds is `AlreadyExists`.
pub const NAMESPACE_KEEP_ALL: u32 = 1 << 0;
/// Every bit [`NamespaceBuild::flags`] defines. A caller that sets another is
/// refused `InvalidArgument`, never served the request it did not make.
pub const NAMESPACE_FLAGS_KNOWN: u32 = NAMESPACE_KEEP_ALL;

/// Names one namespace may bind. Policy on the primitive; a caller asking for
/// one more is refused by name and never truncated.
pub const MAX_NAMESPACE_ENTRIES: usize = 64;
/// Bytes in one service name.
pub const MAX_SERVICE_NAME: usize = 64;

/// # Safety
/// Every pointer in `args` must name `args`'s stated length of readable memory.
pub unsafe fn namespace_build(args: &NamespaceBuild) -> Result<RawHandle, SyscallError> {
    check(syscall(SYS_NAMESPACE_BUILD, args as *const _ as u64, 0, 0, 0))
        .map(|v| RawHandle(v as u32))
}

/// Open a connection to `name` in the namespace `ns` holds.
///
/// `NotFound` means the name is not in this namespace — a fact about this
/// process. [`SyscallError::Gone`] means the server that held the acceptor has
/// exited. There is no third answer, and in particular there is no "not yet":
/// the port exists before either process runs.
pub fn namespace_open(ns: RawHandle, name: &str) -> Result<RawHandle, SyscallError> {
    check(syscall(
        SYS_NAMESPACE_OPEN,
        ns.0 as u64,
        name.as_ptr() as u64,
        name.len() as u64,
        0,
    ))
    .map(|v| RawHandle(v as u32))
}

/// Bytes one badge may carry. Policy on the primitive: a badge is kernel
/// memory no process is charged for, so the bound is what its one reader, the
/// supervisor's launch authority, needs.
pub const MAX_BADGE: usize = 64;

/// Mint a connector to the port `acceptor` accepts on, whose every connection
/// is stamped with `badge`: 1 to [`MAX_BADGE`] bytes, anything else
/// `InvalidArgument`.
///
/// **The kernel vouches for what was granted, never for who connects**: the
/// connector is duplicated, moved and put in a namespace like any other, and
/// every copy stamps the same bytes.
pub fn port_mint(acceptor: RawHandle, badge: &[u8]) -> Result<RawHandle, SyscallError> {
    check(syscall(SYS_PORT_MINT, acceptor.0 as u64, badge.as_ptr() as u64, badge.len() as u64, 0))
        .map(|v| RawHandle(v as u32))
}

/// The badge `connection`, accepted from `acceptor`'s port, was stamped with,
/// written to `out`; answers its length.
///
/// `NotFound` is a connection made through an unbadged connector,
/// `PermissionDenied` one accepted from another port, and `InvalidArgument` a
/// handle that is no port's accepted end.
pub fn port_badge(
    acceptor: RawHandle,
    connection: RawHandle,
    out: &mut [u8; MAX_BADGE],
) -> Result<usize, SyscallError> {
    check(syscall(SYS_PORT_BADGE, acceptor.0 as u64, connection.0 as u64, out.as_mut_ptr() as u64, 0))
        .map(|v| v as usize)
}

/// Accept a queued connection. Blocks until there is one.
///
/// **It answers with the connection and nothing else.** Who connected is not
/// the kernel's to assert: a server that wants to name its client reads it out
/// of the protocol's first frame, where it is already the client's own claim
/// about itself and already distrusted.
pub fn accept(acceptor: RawHandle) -> Result<RawHandle, SyscallError> {
    check(syscall(SYS_ACCEPT, acceptor.0 as u64, 0, 0, 0)).map(|v| RawHandle(v as u32))
}

/// [`accept`]'s flag: answer `WouldBlock` rather than wait when none is queued.
pub const ACCEPT_NONBLOCK: u64 = 1;

/// [`accept`] of a connection already queued; `WouldBlock` when none is.
pub fn accept_nonblock(acceptor: RawHandle) -> Result<RawHandle, SyscallError> {
    check(syscall(SYS_ACCEPT, acceptor.0 as u64, ACCEPT_NONBLOCK, 0, 0)).map(|v| RawHandle(v as u32))
}

/// Handles one [`message_send`] may carry.
///
/// Policy on the primitive: a caller asking for one more is refused by name
/// and never truncated to fit.
pub const MAX_TRANSFER_HANDLES: usize = 8;

/// Messages one direction of a connection may hold unreceived.
pub const MAX_QUEUED_MESSAGES: usize = 256;

/// Handles one direction of a connection may hold unreceived, across its
/// messages.
pub const MAX_QUEUED_HANDLES: usize = 64;

/// The most bytes one message carries. Policy on the primitive: the bytes are
/// copied whole under the connection's lock, so the copy is bounded, and bulk
/// data travels in shared memory.
pub const MAX_MESSAGE_LEN: usize = 16 * 1024;

/// [`message_send`] or [`message_recv`] answers `WouldBlock` rather than
/// waiting.
pub const MESSAGE_NONBLOCK: u64 = 1;

crate::user_safe! {
    /// Arguments for `SYS_MESSAGE_SEND`, passed as a single pointer.
    #[derive(Clone, Copy)]
    pub struct MessageSend {
        pub bytes: u64,
        pub len: u64,
        pub handles: u64,
        pub count: u64,
        /// [`MESSAGE_NONBLOCK`] or none; any other bit is `InvalidArgument`.
        pub flags: u64,
    }
}

impl MessageSend {
    fn new(bytes: &[u8], handles: &[RawHandle], nonblock: bool) -> Self {
        Self {
            bytes: bytes.as_ptr() as u64,
            len: bytes.len() as u64,
            handles: handles.as_ptr() as u64,
            count: handles.len() as u64,
            flags: if nonblock { MESSAGE_NONBLOCK } else { 0 },
        }
    }
}

/// Queue `bytes` and `handles` on `conn` as one message for its peer.
///
/// **All or nothing, and in order.** The peer takes each message whole, its
/// bytes and exactly its handles, in the order they were sent
/// ([`message_recv`]). Nothing is queued and no handle moves on a refusal:
/// - every handle must carry [`Rights::TRANSFER`], and so must `conn` when
///   there are any; `conn` itself and a handle named twice are refused;
/// - past [`MAX_MESSAGE_LEN`] bytes or [`MAX_TRANSFER_HANDLES`] handles is
///   `InvalidArgument`;
/// - a peer that is gone is `Gone`, and memory for the bytes the machine
///   does not have is `ResourceExhausted`;
/// - without room for the whole message beside the peer's unreceived ones
///   (at most [`MAX_QUEUED_MESSAGES`], [`MAX_QUEUED_HANDLES`] and
///   [`PIPE_CAPACITY`] bytes) it waits for room, or with `nonblock` answers
///   `WouldBlock`. `conn` polls writable once the largest message fits.
///
/// [`Rights::TRANSFER`]: crate::handle::Rights::TRANSFER
///
/// # Safety
/// Each of `handles` is the caller's to give away: a sent handle leaves this
/// process, so nothing may answer for it afterwards ([`crate::handle`]).
pub unsafe fn message_send(conn: RawHandle, bytes: &[u8], handles: &[RawHandle], nonblock: bool) -> Result<(), SyscallError> {
    let args = MessageSend::new(bytes, handles, nonblock);
    check_unit(syscall(SYS_MESSAGE_SEND, conn.0 as u64, &args as *const MessageSend as u64, 0, 0))
}

crate::user_safe! {
    /// Arguments for `SYS_MESSAGE_RECV`, passed as a single pointer: where
    /// the message's bytes go, its first `head_len` to `head` and the rest to
    /// `bytes`, and where its handles go.
    #[derive(Clone, Copy)]
    pub struct MessageRecv {
        pub head: u64,
        pub head_len: u64,
        pub bytes: u64,
        pub len: u64,
        pub handles: u64,
        pub cap: u64,
        /// [`MESSAGE_NONBLOCK`] or none; any other bit is `InvalidArgument`.
        pub flags: u64,
    }
}

impl MessageRecv {
    fn new(head: &mut [u8], bytes: &mut [u8], handles: &mut [RawHandle], nonblock: bool) -> Self {
        Self {
            head: head.as_mut_ptr() as u64,
            head_len: head.len() as u64,
            bytes: bytes.as_mut_ptr() as u64,
            len: bytes.len() as u64,
            handles: handles.as_mut_ptr() as u64,
            cap: handles.len() as u64,
            flags: if nonblock { MESSAGE_NONBLOCK } else { 0 },
        }
    }
}

/// A message taken: how many bytes and handles it carried.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Received {
    pub len: usize,
    pub handles: usize,
}

impl Received {
    /// The answer's word: the length low, the handle count above it.
    fn decode(word: u64) -> Self {
        Self { len: (word & 0xFFFF_FFFF) as usize, handles: (word >> 32) as usize }
    }

    /// The word the kernel answers.
    pub const fn encode(len: usize, handles: usize) -> u64 {
        len as u64 | (handles as u64) << 32
    }
}

const _: () = assert!(Received::encode(MAX_MESSAGE_LEN, MAX_TRANSFER_HANDLES) < u64::MAX - 255);

/// Take `conn`'s next message whole: its first bytes into `head`, the rest
/// into `bytes`, and its handles, each installed in this process's table,
/// into `handles`. Two places, so a protocol's own header and the payload
/// behind it each land where the caller keeps them, with no copy between.
///
/// Waits for one unless `nonblock`, which answers `WouldBlock`. Once the peer
/// has gone and nothing is queued, `Gone`. **A message is never cut**: one
/// larger than `head` and `bytes` together, or carrying more handles than
/// `handles` holds, is `InvalidArgument`, and a table without room for its
/// handles `ResourceExhausted`, both leaving it queued. A protocol bounds its
/// own messages, so either is the peer breaking it.
pub fn message_recv(
    conn: RawHandle,
    head: &mut [u8],
    bytes: &mut [u8],
    handles: &mut [RawHandle],
    nonblock: bool,
) -> Result<Received, SyscallError> {
    let args = MessageRecv::new(head, bytes, handles, nonblock);
    check(syscall(SYS_MESSAGE_RECV, conn.0 as u64, &args as *const MessageRecv as u64, 0, 0)).map(Received::decode)
}

/// Make a shared-memory region of zeroed memory and answer a handle to it.
///
/// Fallible: a size the kernel cannot express in whole pages is
/// `InvalidArgument` and memory it does not have is `ResourceExhausted`. A
/// daemon reaches both through a client's request, so neither may be an
/// assertion here.
pub fn shm_create(size: usize) -> Result<RawHandle, SyscallError> {
    shm_create_flags(size, 0)
}

/// [`shm_create`], with [`SHM_DEVICE`] for memory a device is lent.
pub fn shm_create_flags(size: usize, flags: u64) -> Result<RawHandle, SyscallError> {
    check(syscall(SYS_SHM_CREATE, size as u64, flags, 0, 0)).map(|v| RawHandle(v as u32))
}

/// [`shm_create`], counted to the job `job` names (carrying `WRITE`) rather
/// than the caller's: memory a launcher lays out for a child it starts in
/// that job, such as its log ring.
pub fn shm_create_for(job: RawHandle, size: usize) -> Result<RawHandle, SyscallError> {
    check(syscall(SYS_SHM_CREATE, size as u64, SHM_FOR_JOB, job.0 as u64, 0)).map(|v| RawHandle(v as u32))
}

/// Map the region `shm` names into this process as `prot` says, and answer
/// where: [`VmProt::Read`] needs [`Rights::MAP`], [`VmProt::ReadWrite`] needs
/// [`Rights::WRITE`] beside it, and any other protection is `InvalidArgument`.
/// A read-only mapping never becomes writable ([`vm_protect`]).
///
/// Each call is a mapping of its own, which holds the region until it is taken
/// away ([`munmap`]) or the process ends: closing a handle takes no mapping
/// away, but for a device's window, which goes from every process with its
/// last handle. [`SyscallError::Gone`] for such a window whose last handle was
/// closed under the call: nothing is mapped.
///
/// [`Rights::MAP`]: crate::handle::Rights::MAP
/// [`Rights::WRITE`]: crate::handle::Rights::WRITE
///
/// # Safety
/// Caller must manage the returned pointer.
pub unsafe fn shm_map(shm: RawHandle, prot: VmProt) -> Result<*mut u8, SyscallError> {
    check(syscall(SYS_SHM_MAP, shm.0 as u64, prot as u64, 0, 0))
        .map(|addr| core::ptr::with_exposed_provenance_mut(addr as usize))
}

/// A memory object over `len` bytes of the one `shm` names, from `offset`: the
/// same pages, not a copy, so it is written by nobody when the original is
/// written by nobody. The new handle carries `shm`'s rights. Needs
/// [`Rights::MAP`]. `InvalidArgument` for an `offset` that is no whole number
/// of [`PAGE_SIZE`]s, no bytes, a range past the object's bytes, or a device's
/// window.
///
/// [`Rights::MAP`]: crate::handle::Rights::MAP
///
/// # Safety
/// As [`dup`]'s: the new handle reaches `shm`'s pages with its rights.
pub unsafe fn shm_sub(shm: RawHandle, offset: u64, len: u64) -> Result<RawHandle, SyscallError> {
    check(syscall(SYS_SHM_SUB, shm.0 as u64, offset, len, 0)).map(|v| RawHandle(v as u32))
}

/// Say whether the holder of `console` carries the machine's log off it by
/// another channel, so the kernel puts on the console UART only a fatal
/// path's and the stop's lines; the console's end lets go of it.
///
/// Needs [`Rights::WRITE`]. `AlreadyExists` while another holder carries it,
/// and `NotSupported` once a fatal path or the stop has given the log to the
/// UART for good; letting go always answers `Ok`.
///
/// [`Rights::WRITE`]: crate::handle::Rights::WRITE
pub fn console_carried(console: RawHandle, elsewhere: bool) -> Result<(), SyscallError> {
    check(syscall(SYS_CONSOLE_CARRIED, console.0 as u64, elsewhere as u64, 0, 0)).map(drop)
}

/// The machine's [header](SYSINFO_HEADER_SIZE), then as much of the process
/// roster as `buf` has room for. Answers the number of bytes written.
///
/// **`syscap` is consulted only for the roster.** A `buf` shorter than
/// `SYSINFO_HEADER_SIZE + SYSINFO_ENTRY_SIZE` cannot hold an entry, so nothing
/// is demanded and [`HANDLE_INVALID`] is what a header-only caller passes —
/// which is what [`crate::handle::Rights::ROSTER`] leaves ambient. A longer
/// `buf` is a request for the roster, and the ordinary handle rules apply to
/// `syscap`: a capability without the bit is refused with a word, and a handle
/// the caller does not hold ends it.
///
/// [`HANDLE_INVALID`]: crate::handle::HANDLE_INVALID
pub fn sysinfo(syscap: RawHandle, buf: &mut [u8]) -> usize {
    let n = syscall(
        SYS_SYSINFO,
        syscap.0 as u64,
        buf.as_mut_ptr() as u64,
        buf.len() as u64,
        0,
    );
    if SyscallError::from_u64(n).is_some() { 0 } else { n as usize }
}

/// Every [`crate::inventory`] record the machine has, into `buf`; answers how
/// many were written.
///
/// **Whole or not at all.** A `buf` with room for fewer records than there
/// are is refused with [`SyscallError::ResourceExhausted`] and nothing is
/// written; an empty `buf` is the question "how many", answered without the
/// records. Between the two calls the machine can change, so a caller sizes
/// from the answer and asks again when refused.
///
/// `syscap` must carry [`crate::handle::Rights::INVENTORY`]: a capability
/// without it is refused with a word, and a handle the caller does not hold
/// ends it.
pub fn device_inventory(
    syscap: RawHandle,
    buf: &mut [crate::inventory::RawRecord],
) -> Result<usize, SyscallError> {
    check(syscall(
        SYS_DEVICE_INVENTORY,
        syscap.0 as u64,
        buf.as_mut_ptr() as u64,
        buf.len() as u64,
        0,
    ))
    .map(|n| n as usize)
}

/// The [`crate::boot::BootSources`] the loader handed the kernel, unchanged.
///
/// `syscap` must carry [`crate::handle::Rights::INVENTORY`]: a capability
/// without it is refused with a word, and a handle the caller does not hold
/// ends it.
pub fn boot_sources(syscap: RawHandle) -> Result<crate::boot::BootSources, SyscallError> {
    let mut sources = crate::boot::BootSources::NONE;
    check_unit(syscall(SYS_BOOT_SOURCES, syscap.0 as u64, core::ptr::from_mut(&mut sources) as u64, 0, 0))
        .map(|()| sources)
}

/// One [`crate::counters`] record per online CPU, into `buf`; answers how many
/// were written. An empty `buf` asks how many CPUs there are and asks no CPU
/// anything; one with room for fewer is refused with
/// [`SyscallError::ResourceExhausted`], and one declared longer than the
/// kernel's most CPUs with [`SyscallError::InvalidArgument`].
///
/// `syscap` must carry [`crate::handle::Rights::COUNTERS`]: a capability
/// without it is refused with a word, and a handle the caller does not hold
/// ends it.
pub fn counters(
    syscap: RawHandle,
    buf: &mut [crate::counters::RawRecord],
) -> Result<usize, SyscallError> {
    check(syscall(SYS_COUNTERS, syscap.0 as u64, buf.as_mut_ptr() as u64, buf.len() as u64, 0))
        .map(|n| n as usize)
}

/// Copy the diary's records `cursor` has not seen into `out`, oldest first and
/// merged across CPUs by stamp, advancing `cursor`. Answers how many were
/// written, and `0` when there is nothing new; it never blocks.
///
/// `cursor` and `out` are as [`log_read`]'s, over the diary's rings, a ring
/// per CPU the firmware names, and refused as its are. `syscap` must carry
/// [`crate::handle::Rights::TRACE`]: a capability without it is refused with
/// a word, and a handle the caller does not hold ends it.
pub fn trace_read(
    syscap: RawHandle,
    cursor: &mut [u64],
    out: &mut [crate::trace::TraceRecord],
) -> Result<usize, SyscallError> {
    let cursor = sized_cursor(cursor)?;
    check(syscall(SYS_TRACE_READ, syscap.0 as u64, cursor as u64, out.as_mut_ptr() as u64, out.len() as u64))
        .map(|n| n as usize)
}

/// Have the kernel make one access on the `acpi` claim `claim`, or refuse it
/// by name. `Ok` is what a read answered, and what a write wrote.
///
/// The claim must have been read once, which binds it to the caller: one that
/// was not is refused [`SyscallError::PermissionDenied`], and an access that
/// names no space or width [`SyscallError::InvalidArgument`].
pub fn acpi_access(
    claim: RawHandle,
    access: &mut crate::acpi::Access,
) -> Result<Result<u64, crate::acpi::Refused>, SyscallError> {
    check_unit(syscall(SYS_ACPI, claim.0 as u64, crate::acpi::op::ACCESS, access as *mut crate::acpi::Access as u64, 0))?;
    Ok(match access.refused {
        0 => Ok(access.value),
        word => Err(crate::acpi::Refused::from_raw(word).expect("the kernel answers a refusal this ABI names")),
    })
}

/// Try the firmware's Global Lock for the `acpi` claim's holder: `Ok(true)`
/// where it is now held, `Ok(false)` where the firmware owns it and will raise
/// `GBL_STS` on letting go. A lock the claim already holds is refused
/// [`SyscallError::AlreadyExists`], and every take
/// [`SyscallError::NotSupported`] on a machine whose FADT names a FACS the
/// kernel exchanges no lock word in; one that names no FACS has no lock, and
/// a take is answered taken.
pub fn acpi_lock_take(claim: RawHandle) -> Result<bool, SyscallError> {
    check(syscall(SYS_ACPI, claim.0 as u64, crate::acpi::op::LOCK_TAKE, 0, 0)).map(|word| word == crate::acpi::op::TAKEN)
}

/// Give the Global Lock back. One the claim does not hold is refused
/// [`SyscallError::InvalidArgument`].
pub fn acpi_lock_release(claim: RawHandle) -> Result<(), SyscallError> {
    check_unit(syscall(SYS_ACPI, claim.0 as u64, crate::acpi::op::LOCK_RELEASE, 0, 0))
}

/// Supply the `SLP_TYPa` of the machine's `\_S5` for the kernel's power-off
/// ([`crate::acpi::op::S5`]). One wider than the register's three bits is
/// refused [`SyscallError::InvalidArgument`] and a second under one claim
/// [`SyscallError::AlreadyExists`], and nothing is kept of either.
pub fn acpi_s5(claim: RawHandle, slp_typ_a: u64) -> Result<(), SyscallError> {
    check_unit(syscall(SYS_ACPI, claim.0 as u64, crate::acpi::op::S5, slp_typ_a, 0))
}

/// Declare one processor's idle states ([`crate::acpi::op::IDLE`]).
/// `InvalidArgument` is a declaration the kernel will not enter on that
/// processor, `NotFound` a UID that names no CPU it runs, and its log line
/// says which; nothing of a refused one is kept.
pub fn acpi_idle(claim: RawHandle, declaration: &crate::acpi::IdleDeclaration) -> Result<(), SyscallError> {
    check_unit(syscall(SYS_ACPI, claim.0 as u64, crate::acpi::op::IDLE, declaration as *const crate::acpi::IdleDeclaration as u64, 0))
}

/// Sleep for the given number of nanoseconds.
pub fn nanosleep(nanos: u64) {
    syscall(SYS_NANOSLEEP, nanos, 0, 0, 0);
}

/// What [`SYS_HANDLE_DUP`]'s rights word carries when the caller wants the
/// source's own set.
///
/// A wire encoding of `Option<Rights>`, decoded at the syscall boundary and
/// never carried inward: `Rights::ALL` fits in sixteen bits, so this value is not a
/// rights set and never becomes one. The two wrappers below are the only
/// writers, so no caller ever spells it.
pub const RIGHTS_UNCHANGED: u64 = u64::MAX;

/// A second handle to the same object, carrying what the first carries.
///
/// # Safety
/// `handle` is live in this process for the call, and nothing that answers
/// for it relies on being the object's only holder ([`crate::handle`]).
pub unsafe fn dup(handle: RawHandle) -> Result<RawHandle, SyscallError> {
    check(syscall(SYS_HANDLE_DUP, handle.0 as u64, RIGHTS_UNCHANGED, 0, 0))
        .map(|v| RawHandle(v as u32))
}

/// A second handle to the same object, carrying **less**.
///
/// `PermissionDenied` for a set the source does not itself hold: rights only
/// shrink, and asking to widen is a bug in the asker rather than a request to
/// be quietly cut down to size. This is how the supervisor hands a program an `RT`-only
/// `SysCap` while keeping the full one.
///
/// # Safety
/// As [`dup`]'s.
pub unsafe fn dup_narrowed(handle: RawHandle, rights: Rights) -> Result<RawHandle, SyscallError> {
    check(syscall(SYS_HANDLE_DUP, handle.0 as u64, rights.bits() as u64, 0, 0))
        .map(|v| RawHandle(v as u32))
}

/// A fresh null handle carrying `rights`: a read answers 0 bytes, a write of
/// n bytes answers n, and a poll is ready at once in either direction. It
/// names nothing and so carries no authority; `rights` may be any subset of
/// `READ | WRITE | DUP | TRANSFER | WAIT`, and anything else is refused
/// `InvalidArgument`.
pub fn null(rights: Rights) -> Result<RawHandle, SyscallError> {
    check(syscall(SYS_NULL, rights.bits() as u64, 0, 0, 0)).map(|v| RawHandle(v as u32))
}

/// Get the current process ID.
pub fn getpid() -> Pid {
    Pid(syscall(SYS_GETPID, 0, 0, 0, 0) as u32)
}

/// Rename a file.
pub fn rename(old: &[u8], new: &[u8]) -> Result<(), SyscallError> {
    check_unit(syscall(SYS_RENAME, old.as_ptr() as u64, old.len() as u64, new.as_ptr() as u64, new.len() as u64))
}

/// Create a directory.
pub fn mkdir(path: &[u8]) -> Result<(), SyscallError> {
    check_unit(syscall(SYS_MKDIR, path.as_ptr() as u64, path.len() as u64, 0, 0))
}

/// Remove a directory.
pub fn rmdir(path: &[u8]) -> Result<(), SyscallError> {
    check_unit(syscall(SYS_RMDIR, path.as_ptr() as u64, path.len() as u64, 0, 0))
}

/// Create a symbolic link at `link` pointing to `target`.
pub fn symlink(target: &[u8], link: &[u8]) -> Result<(), SyscallError> {
    check_unit(syscall(SYS_SYMLINK, target.as_ptr() as u64, target.len() as u64, link.as_ptr() as u64, link.len() as u64))
}

/// Read the target of a symbolic link. Returns the number of bytes written to `buf`.
pub fn readlink(path: &[u8], buf: &mut [u8]) -> Result<usize, SyscallError> {
    check(syscall(SYS_READLINK, path.as_ptr() as u64, path.len() as u64, buf.as_mut_ptr() as u64, buf.len() as u64)).map(|n| n as usize)
}

/// Libraries one process holds, `DT_NEEDED` and loaded alike; [`dl_open`] of a
/// name it does not hold past it is `ResourceExhausted`.
pub const MAX_LIBRARIES: usize = 1024;

/// Load the shared library `image`'s first `image_len` bytes hold into this
/// process as `name` ([`DlopenArgs`]), and run its `.init_array`.
pub fn dl_open(name: &[u8], image: RawHandle, image_len: u64) -> Result<u64, SyscallError> {
    let mut init_info: [u64; 2] = [0; 2];
    let args = DlopenArgs {
        name_ptr: name.as_ptr() as u64,
        name_len: name.len() as u64,
        image: u64::from(image.0),
        image_len,
        init_out: init_info.as_mut_ptr() as u64,
    };
    let handle = dl_open_args(&args)?;
    // Run .init_array constructors (e.g. EH frame finder registration in cdylib std)
    let init_array_ptr = init_info[0];
    let init_count = init_info[1];
    if init_array_ptr != 0 && init_count > 0 {
        // SAFETY: `init_array_ptr`/`init_count` are not userland's to make
        // up — `SYS_DLOPEN` wrote them, naming the `.init_array` table it
        // just built from the loaded ELF's own section headers, so they name
        // `init_count` valid `usize`-sized, `usize`-aligned entries.
        let entries = unsafe { core::slice::from_raw_parts(init_array_ptr as *const usize, init_count as usize) };
        for &entry in entries {
            if entry != 0 {
                // SAFETY: a non-zero `.init_array` entry is the address of a
                // constructor the loader placed there — by the ELF `DT_INIT_ARRAY`
                // contract every entry is a valid `extern "C" fn()`, never data.
                let f: extern "C" fn() = unsafe { core::mem::transmute(entry) };
                f();
            }
        }
    }
    Ok(handle)
}

/// `SYS_DLOPEN` as `args` say, running nothing it loads: [`dl_open`]'s call,
/// for a caller that names `init_out` itself.
pub fn dl_open_args(args: &DlopenArgs) -> Result<u64, SyscallError> {
    check(syscall(SYS_DLOPEN, args as *const DlopenArgs as u64, 0, 0, 0))
}

/// Look up a symbol in a loaded shared library. Returns the address.
///
/// # Safety
/// The returned address must only be transmuted to the correct function signature.
pub unsafe fn dl_sym(handle: u64, name: &[u8]) -> Result<u64, SyscallError> {
    check(syscall(SYS_DLSYM, handle, name.as_ptr() as u64, name.len() as u64, 0))
}

/// Close a loaded shared library handle.
pub fn dl_close(handle: u64) -> u64 {
    syscall(SYS_DLCLOSE, handle, 0, 0, 0)
}

/// Block if `*addr == expected`. Returns 0 on wake, 1 on timeout.
/// `None` = wait forever, `Some(nanos)` = timeout.
///
/// # Safety
/// `addr` must point to a valid, aligned `u32`.
pub unsafe fn futex_wait(addr: *const u32, expected: u32, timeout: Option<u64>) -> u64 {
    syscall(SYS_FUTEX_WAIT, addr as u64, expected as u64, encode_timeout(timeout), 0)
}

/// Wake up to `count` threads waiting on `addr`. Returns number of threads woken.
///
/// # Safety
/// `addr` must point to a valid, aligned `u32`.
pub unsafe fn futex_wake(addr: *const u32, count: u32) -> u64 {
    syscall(SYS_FUTEX_WAKE, addr as u64, count as u64, 0, 0)
}

/// Truncate a file handle to `size` bytes.
pub fn ftruncate(handle: RawHandle, size: u64) -> Result<(), SyscallError> {
    check_unit(syscall(SYS_FTRUNCATE, handle.0 as u64, size, 0, 0))
}

/// Get the current thread's stack base address and size.
pub fn stack_info() -> Option<(u64, u64)> {
    let mut base: u64 = 0;
    let mut size: u64 = 0;
    let r = syscall(SYS_STACK_INFO, &mut base as *mut u64 as u64, &mut size as *mut u64 as u64, 0, 0);
    if SyscallError::from_u64(r).is_some() { None } else { Some((base, size)) }
}

/// Return the number of available CPUs.
pub fn cpu_count() -> u32 {
    syscall(SYS_CPU_COUNT, 0, 0, 0, 0) as u32
}

/// `len` bytes of private memory, rounded up to whole [`PAGE_SIZE`]s, placed
/// as `at` says. Any `prot` but `None` commits the pages: they read as zeros
/// and are populated as they are touched. `None` reserves them, and
/// [`vm_protect`] commits them later.
///
/// # Safety
/// A `Fixed` range replaces whatever those pages held, so nothing may still
/// reach that.
pub unsafe fn vm_reserve(len: usize, prot: VmProt, at: VmAt) -> Result<*mut u8, SyscallError> {
    let (place, kind) = match at {
        VmAt::Anywhere { align } => (align as u64, VM_AT_ANYWHERE),
        VmAt::Fixed(addr) => (addr as u64, VM_AT_FIXED),
    };
    check(syscall(SYS_VM_RESERVE, len as u64, prot as u64, place, kind))
        .map(|addr| core::ptr::with_exposed_provenance_mut(addr as usize))
}

/// Ranges one address space holds, each a run of pages with one kind and
/// protection: a call that could pass it is `ResourceExhausted`.
pub const MAX_RANGES: usize = 32_768;

/// Take away every page of `[addr, addr + len)`, as POSIX's `munmap` does: any
/// run of whole pages, across ranges and the holes between them. `addr` off a
/// page, or a `len` of zero, is `InvalidArgument` and takes nothing; so does a
/// space holding nearly [`MAX_RANGES`], `ResourceExhausted`, since cutting a
/// range in two adds one.
///
/// # Safety
/// Nothing reaches those pages after this answers `Ok`.
pub unsafe fn vm_release(addr: *mut u8, len: usize) -> Result<(), SyscallError> {
    check_unit(syscall(SYS_VM_RELEASE, addr as u64, len as u64, 0, 0))
}

/// Give every page of `[addr, addr + len)` the protection `prot`, as POSIX's
/// `mprotect` does. A reserved private page becomes committed unless `prot` is
/// `None`; a committed one keeps its contents and its commitment whatever
/// `prot` is. A page of a shared object takes no protection past what it was
/// mapped with. Every page must be held (`BadAddress` otherwise), and either
/// every page changes or none does.
///
/// # Safety
/// No live reference into the range relies on an access `prot` takes away.
pub unsafe fn vm_protect(addr: *mut u8, len: usize, prot: VmProt) -> Result<(), SyscallError> {
    check_unit(syscall(SYS_VM_PROTECT, addr as u64, len as u64, prot as u64, 0))
}

/// Free the frames behind every page of `[addr, addr + len)` and keep the
/// pages committed: the next touch of a private page reads zeros, as Linux's
/// `MADV_DONTNEED` makes it. Every page must be held.
///
/// # Safety
/// Nothing relies on the private pages' contents.
pub unsafe fn vm_discard(addr: *mut u8, len: usize) -> Result<(), SyscallError> {
    check_unit(syscall(SYS_VM_DISCARD, addr as u64, len as u64, 0, 0))
}

/// Free the frames and the commitment of every page of `[addr, addr + len)`,
/// which must all be private memory: they are reserved again, faulting on any
/// access until [`vm_protect`] commits them.
///
/// # Safety
/// Nothing reaches those pages meanwhile.
pub unsafe fn vm_decommit(addr: *mut u8, len: usize) -> Result<(), SyscallError> {
    check_unit(syscall(SYS_VM_DECOMMIT, addr as u64, len as u64, 0, 0))
}

/// Resize `[addr, addr + old_len)`, committed private memory of one
/// protection that is not executable, to `new_len` bytes, both rounded up to
/// whole pages: its pages keep their contents and a grown tail reads as
/// zeros. It grows in place where the pages after it are free; otherwise, if
/// `may_move`, its pages move to a new place, unmapped at the old, with
/// nothing copied. A shrink gives back the tail in place.
/// Answers where the range is now. Refused, it is left as it was:
/// `BadAddress` for a page no range holds, `InvalidArgument` for any other
/// memory or a zero length, and `ResourceExhausted` where it has no room.
///
/// # Safety
/// Nothing reaches the old pages after this answers a new place, nor past
/// `new_len` after a shrink.
pub unsafe fn vm_remap(addr: *mut u8, old_len: usize, new_len: usize, may_move: bool) -> Result<*mut u8, SyscallError> {
    check(syscall(SYS_VM_REMAP, addr as u64, old_len as u64, new_len as u64, may_move as u64))
        .map(|at| core::ptr::with_exposed_provenance_mut(at as usize))
}

/// Non-blocking read. Returns bytes read, or `Err(WouldBlock)` if no data available.
pub fn read_nonblock(handle: RawHandle, buf: &mut [u8]) -> Result<usize, SyscallError> {
    check(syscall(SYS_READ_NONBLOCK, handle.0 as u64, buf.as_mut_ptr() as u64, buf.len() as u64, 0)).map(|n| n as usize)
}

/// Non-blocking write. Returns bytes written, `Err(WouldBlock)` if no space is
/// available, or [`SyscallError::Gone`] if the other end has closed. A pipe
/// write of at most [`MAX_WHOLE_PIPE_WRITE`] bytes writes all of them or
/// answers `WouldBlock`.
pub fn write_nonblock(handle: RawHandle, buf: &[u8]) -> Result<usize, SyscallError> {
    check(syscall(SYS_WRITE_NONBLOCK, handle.0 as u64, buf.as_ptr() as u64, buf.len() as u64, 0)).map(|n| n as usize)
}


/// Map one memory BAR of a claimed PCI function, as an object `SYS_SHM_MAP`
/// maps read/write and uncacheable.
///
/// `InvalidArgument` for an index that names no memory BAR.
///
/// Every call answers an object of its own over the same window, so a BAR
/// whose handle was closed can be asked for again.
///
/// A window alone drives nothing: the function masters the bus from
/// [`device_dma_start`] and not before.
pub fn device_bar_map(claim: RawHandle, bar: u32) -> Result<RawHandle, SyscallError> {
    check(syscall(SYS_DEVICE_BAR_MAP, claim.0 as u64, bar as u64, 0, 0))
        .map(|v| RawHandle(v as u32))
}

/// A DMA buffer for a claimed PCI function: memory in this process's address
/// space and in that function's own at the unit, and in no other's.
///
/// An address the caller invents instead of the one this answers is one the
/// function's domain does not map, and the unit refuses the access. `bytes` is
/// rounded up to whole 2 MiB pages, and the grant ends below the claim's
/// [`device_dma_limit`]: `InvalidArgument` before one is stated, and
/// `ResourceExhausted` once nothing below it is left.
pub fn device_dma_alloc(claim: RawHandle, bytes: u64) -> Result<DmaGrant, SyscallError> {
    let mut grant =
        DmaGrant { shm: HANDLE_INVALID, _pad: 0, device_addr: 0, bytes: 0 };
    check_unit(syscall(
        SYS_DEVICE_DMA_ALLOC,
        claim.0 as u64,
        bytes,
        &mut grant as *mut DmaGrant as u64,
        0,
    ))?;
    Ok(grant)
}

/// State that the function `claim` holds issues device addresses of `bits`
/// bits, 32 to 64: every grant and lend of the claim is placed below `1 <<
/// bits`. Once per claim, before any grant: `AlreadyExists` for a second, and
/// `InvalidArgument` outside that range.
///
/// What the device's own capability bits say where they can be trusted, and
/// the driver's own statement where they cannot: a function that drops its
/// upper bits issues an address above its width as another grant's.
pub fn device_dma_limit(claim: RawHandle, bits: u8) -> Result<(), SyscallError> {
    check_unit(syscall(SYS_DEVICE_DMA_LIMIT, claim.0 as u64, bits as u64, 0, 0))
}

/// Let the function `claim` holds master the bus; nothing on a second call.
///
/// **The function issues no bus transaction at all, an MSI-X message
/// included, until this is called.** A claim is handed over with Bus Master
/// Enable clear, because a previous holder or firmware may have left it
/// running queues at addresses its domain no longer maps; its driver stops and
/// resets it through its registers first, lays out its grants, and then starts
/// it. What it does from then on reaches only what its domain maps.
pub fn device_dma_start(claim: RawHandle) -> Result<(), SyscallError> {
    check_unit(syscall(SYS_DEVICE_DMA_START, claim.0 as u64, 0, 0, 0))
}

/// Put the region `shm` names into the address space of the function `claim`
/// holds, and answer where the function reaches it: the region's whole length,
/// read and write, and nothing on either side of it. `shm` must carry
/// [`Rights::WRITE`](crate::handle::Rights::WRITE), as a writable mapping does.
///
/// For a driver serving a client out of the client's own memory — the client
/// sends the region, and the device moves data straight into it. The region
/// stays alive for as long as it is mapped, whoever still holds a handle, and
/// comes back out at [`device_dma_unmap`] or when the claim ends.
///
/// `InvalidArgument` for a region that is not ordinary memory this kernel
/// allocated — a BAR window, firmware's framebuffer — and for one already
/// mapped for this claim; `ResourceExhausted` with no run of the region's
/// length left in the claim's [`LEND_WINDOW`](crate::pci::LEND_WINDOW), and
/// `InvalidArgument` before the claim's [`device_dma_limit`] is stated.
///
/// # Safety
/// `shm` is live in this process for the call, and nothing that answers for
/// it relies on the region being written only through this process's own
/// mapping ([`crate::handle`]).
pub unsafe fn device_dma_map(claim: RawHandle, shm: RawHandle) -> Result<DmaMapping, SyscallError> {
    let mut mapping = DmaMapping { device_addr: 0, bytes: 0 };
    check_unit(syscall(
        SYS_DEVICE_DMA_MAP,
        claim.0 as u64,
        shm.0 as u64,
        &mut mapping as *mut DmaMapping as u64,
        0,
    ))?;
    Ok(mapping)
}

/// Take back a region [`device_dma_map`] put at `device_addr`. From the moment
/// this returns the function reaches none of it: an access it still makes
/// there is refused at the unit and recorded against the claim.
///
/// `NotFound` for an address no [`device_dma_map`] of this claim answered —
/// a [`device_dma_alloc`] grant is the claim's for its whole life and is not
/// taken back here.
pub fn device_dma_unmap(claim: RawHandle, device_addr: u64) -> Result<(), SyscallError> {
    check_unit(syscall(SYS_DEVICE_DMA_UNMAP, claim.0 as u64, device_addr, 0, 0))
}

/// Allocate a TLS block for a dlopen'd module on the current thread.
///
/// The block's *virtual* address, which is what the kernel writes into the DTV.
/// `InvalidArgument` for a `module_id` of 0 or one outside the process's module
/// list, `ResourceExhausted` past `DTV_INITIAL_CAPACITY` or when the mapping
/// fails — and every user address is far below where `SyscallError` encodes, so
/// no block is ever read as one.
pub fn tls_alloc_block(module_id: u64) -> Result<u64, SyscallError> {
    check(syscall(SYS_TLS_ALLOC_BLOCK, module_id, 0, 0, 0))
}

crate::user_safe! {
    /// An inbox and where its page of rings is mapped.
    ///
    /// **The inbox owns its page and the kernel maps it at setup**, so the mapping
    /// has no lifetime of its own: it ends when the last handle to the inbox closes.
    #[derive(Clone, Copy)]
    pub struct InboxSetup {
        pub handle: RawHandle,
        pub _pad: u32,
        pub vaddr: u64,
    }
}

/// Create an [`inbox`](crate::inbox) with the given queue depth (a power of
/// two, at most 256), and map its rings.
///
/// # Safety
/// Caller must manage the returned pointer; it stops being mapped when the
/// last handle to the inbox closes.
pub unsafe fn inbox_setup(depth: u32) -> Result<(RawHandle, *mut u8), SyscallError> {
    let mut out = InboxSetup { handle: HANDLE_INVALID, _pad: 0, vaddr: 0 };
    check_unit(syscall(
        SYS_INBOX_SETUP,
        depth as u64,
        &raw mut out as u64,
        0,
        0,
    ))?;
    Ok((out.handle, core::ptr::with_exposed_provenance_mut(out.vaddr as usize)))
}

/// Hand queued submissions to the kernel and/or wait for completions on an
/// inbox.
/// `to_submit`: number of submissions to consume from the submission ring.
/// `min_complete`: block until at least this many completions are available
/// (0 = don't block).
/// `timeout_nanos`: 0 = non-blocking, u64::MAX = block forever, else timeout in nanos.
/// Returns the number of completions available.
pub fn inbox_submit(handle: RawHandle, to_submit: u32, min_complete: u32, timeout_nanos: u64) -> Result<u32, SyscallError> {
    check(syscall(SYS_INBOX_SUBMIT, handle.0 as u64, to_submit as u64, min_complete as u64, timeout_nanos))
        .map(|n| n as u32)
}

// Module info (for stack unwinding / backtraces)

crate::user_safe! {
    /// Information about a loaded module (executable or shared library).
    ///
    /// Buffer layout returned by `SYS_QUERY_MODULES`:
    ///   `[ModuleInfo; count]` followed by packed path strings.
    ///   Each `ModuleInfo::path_offset` is relative to the start of the buffer.
    #[derive(Clone, Copy)]
    pub struct ModuleInfo {
        /// Load base address (bias) of this module.
        pub base: u64,
        /// End of the last mapped segment (base + vaddr_max).
        pub text_end: u64,
        /// Absolute virtual address of `.eh_frame_hdr` (0 if none).
        pub eh_frame_hdr: u64,
        /// Size of `.eh_frame_hdr` in bytes.
        pub eh_frame_hdr_size: u64,
        /// Absolute virtual address of the module's program header table: the
        /// kernel loads no module whose table a `PT_LOAD` does not map.
        pub phdr: u64,
        /// Entries in that table, `e_phnum`: a `u64` so the record has no padding.
        pub phnum: u64,
        /// Byte offset of the module's path string within the buffer.
        pub path_offset: u32,
        /// Length of the path string in bytes.
        pub path_len: u32,
    }
}

/// Query all loaded modules (exe + dlopen'd libs) in the current process.
///
/// Returns the number of **bytes** the description needs, which is a count
/// only of bytes: the records carry packed path strings, so a module count
/// cannot size a retry. `n <= buf.len()` means the description is in the
/// buffer; `n > buf.len()` means nothing was written and `n` is what to
/// allocate. An empty buffer is therefore a size query.
///
/// The records occupy `buf[..records[0].path_offset]` — each module's path is
/// written after the last record, so the first one's `path_offset` is where
/// the array ends.
pub fn query_modules(buf: &mut [u8]) -> Result<usize, SyscallError> {
    check(syscall(SYS_QUERY_MODULES, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0)).map(|n| n as usize)
}

/// Every record of one [`query_modules`] answer with its path, executable
/// first: `answer` is the `n` bytes the call reported.
///
/// A record or path outside `answer` is a kernel that broke the layout above,
/// and panics.
pub fn modules(answer: &[u8]) -> impl Iterator<Item = (ModuleInfo, &[u8])> {
    let size = core::mem::size_of::<ModuleInfo>();
    let record = move |i: usize| {
        let bytes = &answer[i * size..(i + 1) * size];
        // SAFETY: `bytes` holds `size_of::<ModuleInfo>()` bytes, read without
        // an alignment requirement, and every bit pattern of its integer
        // fields is a `ModuleInfo`.
        unsafe { (bytes.as_ptr() as *const ModuleInfo).read_unaligned() }
    };
    let count = if answer.is_empty() { 0 } else { record(0).path_offset as usize / size };
    (0..count).map(move |i| {
        let info = record(i);
        let path = info.path_offset as usize;
        (info, &answer[path..path + info.path_len as usize])
    })
}

crate::user_safe! {
    /// A job's account, as [`SYS_ACCOUNT_INFO`] answers it, in bytes. A
    /// job's memory stays counted to it while an object it made lives, so
    /// an account outlives the processes that ran in it.
    #[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
    pub struct AccountInfo {
        /// Held back for it however short every other job runs.
        pub floor: u64,
        /// Private and object memory its processes committed.
        pub committed: u64,
        /// Kernel memory it caused: heap its objects keep, its page tables,
        /// pipes' bytes, and runs such as memory a device is lent.
        pub kernel: u64,
        /// The most of the two it held at once.
        pub peak: u64,
        /// What it would have held had its last refused request been
        /// granted; zero if none was.
        pub wanted: u64,
        /// How many of its requests were refused for memory.
        pub refusals: u64,
    }
}

crate::user_safe! {
    /// Per-process accounting statistics, as [`SYS_PROCESS_STATS`] answers them.
    #[derive(Clone, Copy, Default)]
    pub struct ProcessStats {
        pub wall_ns: u64,
        pub cpu_ns: u64,
        pub syscall_total: u64,
        pub syscall_total_ns: u64,
        /// Faults served, and the time they took.
        pub faults: u64,
        pub fault_ns: u64,
        /// The process's own pid. Not authority — no syscall takes a pid — but it
        /// is the name a diagnostic prints, and this is where a holder of a handle
        /// reads it.
        pub pid: u32,
        pub _pad: u32,
        pub blocked_io_ns: u64,
        pub blocked_futex_ns: u64,
        pub blocked_pipe_ns: u64,
        pub blocked_ipc_ns: u64,
        pub blocked_other_ns: u64,
        pub runqueue_wait_ns: u64,
        /// The most private memory it held at once, in bytes, and the ranges
        /// it has made.
        pub peak_memory: u64,
        pub alloc_count: u64,
    }
}

/// Read accounting for the process a `Process` handle names, alive or exited.
///
/// Repeatable: the numbers are the object's, so sampling one does not spend it.
pub fn process_stats(proc: RawHandle, stats: &mut ProcessStats) -> Result<(), SyscallError> {
    check_unit(syscall(
        SYS_PROCESS_STATS,
        proc.0 as u64,
        stats as *mut ProcessStats as u64,
        core::mem::size_of::<ProcessStats>() as u64,
        0,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The encoder is the wire, so the test decodes the wire.**
    ///
    /// Not `bytes(&info).len() == size_of::<ModuleInfo>()`, which a padded
    /// struct passes: every field is read back out of the slice at the offset
    /// `#[repr(C)]` puts it at.
    #[test]
    fn module_info_s_bytes_are_the_fields_and_nothing_between_them() {
        let info = ModuleInfo {
            base: 0x1122_3344_5566_7788,
            text_end: 0x2233_4455_6677_8899,
            eh_frame_hdr: 0x3344_5566_7788_99aa,
            eh_frame_hdr_size: 0x44,
            phdr: 0x5566_7788_99aa_bbcc,
            phnum: 0x77,
            path_offset: 0x55,
            path_len: 0x66,
        };
        let b = crate::usersafe::bytes(&info);
        assert_eq!(b.len(), 56);
        assert_eq!(u64::from_ne_bytes(b[0..8].try_into().unwrap()), info.base);
        assert_eq!(u64::from_ne_bytes(b[8..16].try_into().unwrap()), info.text_end);
        assert_eq!(u64::from_ne_bytes(b[16..24].try_into().unwrap()), info.eh_frame_hdr);
        assert_eq!(u64::from_ne_bytes(b[24..32].try_into().unwrap()), info.eh_frame_hdr_size);
        assert_eq!(u64::from_ne_bytes(b[32..40].try_into().unwrap()), info.phdr);
        assert_eq!(u64::from_ne_bytes(b[40..48].try_into().unwrap()), info.phnum);
        assert_eq!(u32::from_ne_bytes(b[48..52].try_into().unwrap()), info.path_offset);
        assert_eq!(u32::from_ne_bytes(b[52..56].try_into().unwrap()), info.path_len);
    }

    fn record(base: u64, path_offset: u32, path_len: u32) -> ModuleInfo {
        ModuleInfo {
            base,
            text_end: base + 0x1000,
            eh_frame_hdr: 0,
            eh_frame_hdr_size: 0,
            phdr: base + 0x40,
            phnum: 3,
            path_offset,
            path_len,
        }
    }

    /// An answer laid out as the kernel writes one — records, then the paths
    /// packed in module order — decodes to those records and paths, in order,
    /// at an offset whatever alignment the buffer has.
    #[test]
    fn modules_decodes_every_record_and_its_path() {
        let size = core::mem::size_of::<ModuleInfo>() as u32;
        let exe = record(0x100_0000_0000, 2 * size, 9);
        let lib = record(0x200_0000_0000, 2 * size + 9, 13);
        let mut answer = [0u8; 1 + 2 * 56 + 9 + 13];
        let wire = &mut answer[1..];
        wire[..56].copy_from_slice(crate::usersafe::bytes(&exe));
        wire[56..112].copy_from_slice(crate::usersafe::bytes(&lib));
        wire[112..121].copy_from_slice(b"/bin/prog");
        wire[121..].copy_from_slice(b"/lib/libx.so\0");
        let got: [(u64, u64, &[u8]); 2] = {
            let mut it = modules(&answer[1..]).map(|(m, path)| (m.base, m.phdr, path));
            let pair = [it.next().unwrap(), it.next().unwrap()];
            assert!(it.next().is_none(), "two records, and no third read out of the paths");
            pair
        };
        assert_eq!(got[0], (exe.base, exe.phdr, &b"/bin/prog"[..]));
        assert_eq!(got[1], (lib.base, lib.phdr, &b"/lib/libx.so\0"[..]));
        assert_eq!(modules(&[]).count(), 0);
    }

    /// A path the kernel says runs past its own answer is a broken layout,
    /// not a shorter name.
    #[test]
    #[should_panic]
    fn modules_refuses_a_path_past_the_answer() {
        let size = core::mem::size_of::<ModuleInfo>() as u32;
        let mut answer = [0u8; 56 + 4];
        answer[..56].copy_from_slice(crate::usersafe::bytes(&record(0, size, 5)));
        modules(&answer).for_each(drop);
    }

    /// The four readers of a `devices` entry share this parser, so what it
    /// accepts is what a config may say; a PCI function is named by its
    /// arrival, which no entry spells.
    #[test]
    fn a_devices_entry_is_a_class_or_one_named_device() {
        let function = DeviceRequest::Function(crate::inventory::Arrival(7));
        assert_eq!(function.class(), DeviceType::PciFunction);
        assert_eq!(function.selector(), [7, 0]);
        assert_eq!(function.write_name(&mut [0; DeviceRequest::MAX_NAME]), None);
        assert_eq!(
            DeviceRequest::parse("framebuffer"),
            Some(DeviceRequest::Class(DeviceType::Framebuffer))
        );
        assert_eq!(DeviceRequest::Class(DeviceType::Framebuffer).selector(), [0, 0]);
        // Every one of these is a name a config could plausibly write and the
        // kernel must not resolve to a function.
        for bad in [
            "pci", // the bare class name names no function at all
            "pci:1af4:1041", // and a function's matcher is the manifest's
            "gpu",  // a class name this table has never had
            "part", // a bare partition class names no partition
            "part:",
            "part:c12a7328-f81f-11d2-ba4b-00a0c93ec93b", // one spelling: uppercase
            // A partition is named by its unique GUID and nothing else.
            "part-type:C12A7328-F81F-11D2-BA4B-00A0C93EC93B",
            "isa", // a bare ISA class names no ports
            "isa:",
            "isa:0060,0064", // a function with no line
            "isa:0060,0064:",
            "isa::1",
            "isa:60,64:1,12",     // one spelling: four hex digits
            "isa:0064,0060:1,12", // and ascending
            "isa:0060,0060:1",
            "isa:0060:12,1",
            "isa:0060:01",
            "isa:0060:16", // the bus has sixteen lines
            "isa:0000:1",  // port 0 means no port
            "isa:0060,0061,0062,0063,0064:1", // more ports than a selector word holds
            "isa:0060:1,2,3,4,5",
            "isa:0060:+1",
            "",
        ] {
            assert_eq!(DeviceRequest::parse(bad), None, "{bad:?} parsed");
        }
    }

    /// A claim is found again by the label the supervisor wrote it under, and the label's
    /// tail is this name: a round trip that lost a digit would be a claim its
    /// holder cannot look up.
    #[test]
    fn a_request_writes_back_the_name_it_parsed() {
        for name in [
            "framebuffer",
            "isa:0060,0064:1,12",
            "isa:0001:0",
            // The longest entry there is, which is what `MAX_NAME` is for.
            "isa:fff0,fff1,fff2,fffe:12,13,14,15",
        ] {
            let request = DeviceRequest::parse(name).expect("a name this table has");
            let mut buf = [0u8; DeviceRequest::MAX_NAME];
            assert_eq!(request.write_name(&mut buf), Some(name));
        }
    }

    /// An `isa:` entry is its ports in the first selector word and its lines in
    /// the second, and the kernel decodes nothing a config could not have
    /// written: a port lane after a gap, or a line past the bus, is no set.
    #[test]
    fn an_isa_entry_survives_the_wire_and_nothing_else_decodes() {
        let request = DeviceRequest::parse("isa:0060,0064:1,12").expect("the i8042's set");
        let DeviceRequest::Isa(set) = request else { panic!("{request:?} is not an ISA set") };
        assert_eq!(request.class(), DeviceType::Isa);
        assert!(set.ports().eq([0x60, 0x64]));
        assert!(set.irqs().eq([1, 12]));
        assert_eq!(request.selector(), [0x0064_0060, (1 << 1) | (1 << 12)]);
        assert_eq!(IsaId::from_wire(set.wire()), Some(set));
        for bad in [
            [0, 1 << 1],                  // no port
            [0x0060, 0],                  // no line
            [0x0060_0000, 1 << 1],        // a port after a gap
            [0x0060_0064, 1 << 1],        // not ascending
            [0x0060, 1 << 16],            // past the bus
            [0x0060, 0b1_1111],           // more lines than a name holds
            [0x0060_0060, 1 << 1],        // one port twice
        ] {
            assert_eq!(IsaId::from_wire(bad), None, "{bad:x?} decoded");
        }
    }

    /// The id is one word and decodes back; a vendor lost to a shift would
    /// name a different card.
    #[test]
    fn a_pci_id_survives_its_word() {
        for id in [
            PciId { vendor: 0x1af4, device: 0x1041 },
            PciId { vendor: 0x8086, device: 0x15fc },
            PciId { vendor: 0xffff, device: 0xffff },
        ] {
            assert_eq!(PciId::from_wire(id.wire()), Some(id));
        }
        assert_eq!(PciId::from_wire(1 << 32), None);
    }

    /// A listing of `entries`, each a name, whether it is a directory and a
    /// node, encoded as `sys_readdir` (`kernel/src/syscall/fs.rs`) encodes it.
    fn listing(entries: &[(&str, bool, u64)]) -> std::vec::Vec<u8> {
        let mut out = std::vec::Vec::new();
        for (name, is_dir, node) in entries {
            out.push(if *is_dir { 2 } else { 1 });
            out.extend_from_slice(name.as_bytes());
            out.push(0);
            out.extend_from_slice(&node.to_le_bytes());
        }
        out
    }

    fn read_listing(bytes: &[u8]) -> std::vec::Vec<(std::string::String, bool, u64)> {
        let mut at = 0;
        let mut out = std::vec::Vec::new();
        while let Some(entry) = dirent(bytes, &mut at) {
            out.push((std::string::String::from_utf8(entry.name.to_vec()).unwrap(), entry.is_dir, entry.node));
        }
        assert_eq!(at, bytes.len(), "the reader stopped short of the listing's end");
        out
    }

    #[test]
    fn every_dirent_is_read_back_whatever_its_node_holds() {
        // Nodes whose bytes are NULs, kind bytes and letters: a reader that
        // does not step over all eight takes them for names.
        let entries = [
            ("a", false, 0),
            ("dir", true, 0x0201_0000_0000_0000),
            ("b.txt", false, 0x6162_6300_0102_0304),
            ("", false, u64::MAX),
            ("last", true, 17),
        ];
        let want: std::vec::Vec<_> = entries.iter().map(|(n, d, s)| ((*n).into(), *d, *s)).collect();
        assert_eq!(read_listing(&listing(&entries)), want);
        assert!(read_listing(&[]).is_empty());
    }

    #[test]
    #[should_panic(expected = "of kind 3")]
    fn a_dirent_of_another_kind_is_a_broken_kernel() {
        read_listing(&[3, b'x', 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    #[should_panic(expected = "no node")]
    fn a_short_dirent_is_a_broken_kernel() {
        read_listing(&[1, b'x', 0, 0, 0]);
    }
}


// ---------------------------------------------------------------------------
// Paged objects: memory a pager supplies, read by its holders through the kernel.
// ---------------------------------------------------------------------------

/// The most pages one write-back range covers.
pub const PAGED_RANGE_PAGES: u64 = 512;

/// What a pager asks of one of its objects through [`SYS_PAGED_OP`], each
/// with a [`PagedArgs`] naming the object by the id [`paged_create`] answered.
#[repr(u64)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PagedOp {
    /// Copy `len` bytes from `buf` into fresh pages at `offset`, a whole page
    /// in: clean, or dirty with [`PAGED_DIRTY`], which also grows the size to
    /// the end of the bytes. A page already present is left as it is.
    /// Answers the pages installed; `ResourceExhausted` where no page could
    /// be had, `WouldBlock` at the dirty ceiling, `InvalidArgument` for a
    /// clean page past the size or a dirty one past its end.
    Supply = 0,
    /// Copy `len` bytes from `buf` into the present pages at `offset`, dirty,
    /// growing the size to their end as each page's bytes land. Answers the
    /// bytes written, stopping at the first absent page; `NotFound` when the
    /// first is absent, `WouldBlock` at the dirty ceiling.
    Write = 1,
    /// Remove the clean pages of `len` pages from page `offset` that no
    /// write-back or write holds. Answers how many went.
    Withdraw = 2,
    /// Mark `len` pages from page `offset` failed: each read of one answers
    /// `Io` until it is supplied again.
    Fail = 3,
    /// Set the size to `offset`. A shrink drops every page past it, dirty or
    /// not, and zeroes the last page's tail; the new range of a growth reads
    /// as the pager supplies it.
    Resize = 4,
    /// Begin writing back `len` pages from page `offset`, at most
    /// [`PAGED_RANGE_PAGES`]: each dirty page is cleaned and held until the
    /// end. `buf` receives a 64-byte bitmap of the pages begun.
    WritebackBegin = 5,
    /// End exactly a range begun, written to the device or not
    /// ([`PAGED_WRITTEN`]): `buf` receives a 64-byte bitmap of the pages
    /// still dirty, dirtied again since the begin or not written.
    WritebackEnd = 6,
    /// Read `len` bytes at `offset` into `buf`, never waiting: `WouldBlock`
    /// where the first page is absent, a short count where a later one is.
    Read = 7,
    /// Let the object go: its pages go, its holders answer `Gone`, and its id
    /// names another object once its last holder has gone.
    Release = 8,
}

/// [`PagedOp::Supply`]'s pages are dirty.
pub const PAGED_DIRTY: u64 = 1;
/// [`PagedOp::WritebackEnd`]'s range reached the device.
pub const PAGED_WRITTEN: u64 = 1;

crate::user_safe! {
    /// [`SYS_PAGED_OP`]'s arguments: what each word means is its
    /// [`PagedOp`]'s. A byte range — a read's, a supply's, a write's — and a
    /// size end at or below the last page a `u64` can end, or the call is
    /// refused `InvalidArgument`; a page range — a withdrawal's, a failure's,
    /// a write-back's — is clipped to the object.
    #[derive(Clone, Copy, Default, Debug)]
    pub struct PagedArgs {
        pub object: u64,
        pub offset: u64,
        pub len: u64,
        pub buf: u64,
        pub flags: u64,
    }
}

crate::user_safe! {
    /// One thing a pager's object asks of it ([`paged_notices`]): the
    /// object by its id, and either pages a reader waits for or that its last
    /// holder went.
    #[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
    pub struct PagedNotice {
        pub object: u64,
        pub kind: u64,
        /// [`NOTICE_REQUESTED`]'s first page and how many.
        pub first: u64,
        pub count: u64,
    }
}

/// A reader waits for pages of the object.
pub const NOTICE_REQUESTED: u64 = 1;
/// The object's last holder handle went: it may be released.
pub const NOTICE_NO_HOLDERS: u64 = 2;

/// Make this process's one pager, whose objects' pages are at most
/// `capacity`, `dirty` of them dirty at once, and which serves at most
/// `objects` objects. A reader waiting past `report_ns` (bounded by the
/// kernel) is reported overdue and waits on. Committed at once, and charged
/// with the pager: `dirty` pages and the headroom a read's pages need, and
/// the index and object table, which a capacity the job cannot hold is
/// refused for (`ResourceExhausted`). `PermissionDenied` for a process not
/// started as a pager, `AlreadyExists` for a second.
pub fn pager_create(capacity: u64, dirty: u64, objects: u64, report_ns: u64) -> Result<RawHandle, SyscallError> {
    check(syscall(SYS_PAGER_CREATE, capacity, dirty, objects, report_ns)).map(|raw| RawHandle(raw as u32))
}

/// A paged object of `size` bytes, its pages absent: its id, which `pager`'s
/// calls and notices name it by. `ResourceExhausted` where the pager serves
/// as many objects as it was made for.
pub fn paged_create(pager: RawHandle, size: u64) -> Result<u64, SyscallError> {
    check(syscall(SYS_PAGED_CREATE, pager.0 as u64, size, 0, 0))
}

/// A holder handle on `pager`'s object `object`, carrying `READ`, which may
/// be narrowed, duplicated and sent.
pub fn paged_holder(pager: RawHandle, object: u64) -> Result<RawHandle, SyscallError> {
    check(syscall(SYS_PAGED_HOLDER, pager.0 as u64, object, 0, 0)).map(|raw| RawHandle(raw as u32))
}

/// A holder's read of at most `out.len()` bytes at `offset`, up to the size;
/// 0 at or past it. It waits for every absent page until its pager supplies
/// or fails it, the object shrinks or is released, or the pager ends: `Io`
/// for a failed page before any byte was read (a short count after some),
/// `Gone` once the object or the pager ended. A pager's process is refused,
/// `PermissionDenied`, as is a holder without `READ`; any other kind of
/// handle, `InvalidArgument`, so a peer's claim that a handle is a holder
/// can be checked.
pub fn paged_read(holder: RawHandle, out: &mut [u8], offset: u64) -> Result<usize, SyscallError> {
    check(syscall(SYS_PAGED_READ, holder.0 as u64, out.as_mut_ptr() as u64, out.len() as u64, offset)).map(|n| n as usize)
}

/// One [`PagedOp`] on one of `pager`'s objects.
pub fn paged_op(pager: RawHandle, op: PagedOp, args: &PagedArgs) -> Result<u64, SyscallError> {
    check(syscall(SYS_PAGED_OP, pager.0 as u64, op as u64, args as *const PagedArgs as u64, 0))
}

/// What `pager`'s objects ask of it, into `out`: how many it filled. Each is
/// answered once; what does not fit waits for the next call, and the pager
/// reads ready while any does.
pub fn paged_notices(pager: RawHandle, out: &mut [PagedNotice]) -> Result<usize, SyscallError> {
    check(syscall(SYS_PAGED_NOTICES, pager.0 as u64, out.as_mut_ptr() as u64, out.len() as u64, 0)).map(|n| n as usize)
}
