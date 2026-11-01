//! What every program in an image is allowed to hold, written by the build
//! system and read by the supervisor.
//!
//! **One definition of the format, used by both halves.** `build/build.rs`
//! resolves `system.toml` into a [`Manifest`] and [`render`]s it into the
//! ROOT at [`PATH`]; the supervisor [`parse`]s it back. A round-trip test here is what
//! makes that a fact rather than two hand-matched implementations — the shape
//! this crate exists to prevent is a renderer and a parser that disagree about
//! one record and a machine that boots with an authority nobody declared.
//!
//! Line-oriented and deliberately not TOML: the build system already has the
//! parsed config, so a TOML parser in the guest would be a dependency for a
//! job already done.
//!
//! ```text
//! program <name> <path>     starts a program's records: the executable at <path>
//! boot-program <name>       starts one whose process runs the file server's boot image
//! arg <text>                argv after argv[0]
//! serve <name>              the supervisor makes one machine-wide port and endows the acceptor
//! provide <name>            this program makes its own port, once per instance
//! receive <name>            a connector in this program's namespace
//! device <entry>            a claim the supervisor mints and endows ([`device`])
//! drive <matcher>           one process of it per PCI function the matcher names
//! syscap <right>            a right on the SysCap dup the supervisor endows
//! slots                     the idle slot's partitions and the slot table
//! authorize <word>          a disk authorization the supervisor mints at its start ([`authorize`])
//! service                   a system service: its `HOME` is `/state/<name>`, not the session's
//! state                     it keeps machine state of its own in `/state/<name>`
//! role <role>               a file server for `<role>`: one process of it per role
//! restart                   the supervisor starts it again when it ends
//! pager                     started as a pager: it may make the paged objects its files are read through
//! memory-floor <bytes>      memory held back for its job however short every other runs
//! supervisor-floor <bytes>  the same for the supervisor's own job
//! supervisor-serve <name>   a name the supervisor serves itself
//! start <name>              the supervisor starts this program at boot
//! app-receive <name>        a connector every program launched from /apps holds
//! app-folder <access>       the most of a folder of the home a package may be granted
//! starts <key>              a row this program may start through the launcher, or `/apps`
//! login                     a login authority: the supervisor opens login sessions for it
//! ```
//!
//! [`device`] is what a `devices` or `drives` entry names. [`package`] is the
//! other half: what an installed package says about itself,
//! which is which of its own binaries a launch starts and never what it holds.
//! [`launch`] is who may start what, [`grants`] which folder of the home a
//! package is granted, [`consent`] how the person at the screen is asked,
//! [`clipboard`] who watches the clipboard and [`display`] who sets the display.

pub mod authorize;
pub mod clipboard;
pub mod consent;
pub mod device;
pub mod display;
pub mod grants;
pub mod launch;
pub mod package;

/// Where ROOT carries it, without a leading slash — that volume's own
/// spelling. [`GUEST_PATH`] is what a process opens.
pub const PATH: &str = "etc/system.manifest";

/// The path the supervisor opens.
pub const GUEST_PATH: &str = "/system/etc/system.manifest";

/// A program key may be this long. Policy on the primitive: the launcher
/// carries one in a message, and a longer one is refused by name rather than
/// truncated into some other program's.
pub const MAX_PROGRAM_NAME: usize = 32;

/// The port the supervisor serves swaps on: its one holder replaces a
/// running service's binary, so a row receiving it starts only in a login
/// session ([`launch`]).
pub const SWAP_PORT: &str = "swap";

/// The dev image's one user, until the users track gives the supervisor a login row.
pub const USER: &str = "toy";

/// The session user's home: every program's `HOME` that no service row claims,
/// a program no row names included.
pub fn session_home() -> String {
    format!("/home/{USER}")
}

/// An installed package's own folder in the session user's home: its `HOME`,
/// and the one directory of the home its view holds.
pub fn app_home(name: &str) -> String {
    format!("{}/Apps/{name}", session_home())
}

/// Why a launch of the package `name` is refused when a row the image declares
/// has that name: [`app_home`] is a folder that row keeps its own in.
pub fn row_named(name: &str) -> String {
    format!("the package {name} is named after a row the image declares, and {} is that row's folder", app_home(name))
}

/// What an app's own folder holds, made with it: where it keeps its config,
/// data, cache and state. English on disk, as every home folder is.
pub const APP_FOLDERS: [&str; 4] = ["Config", "Data", "Cache", "State"];

/// Where machine state lives, one directory per program key that keeps any
/// ([`Program::state_dir`]); a row's view holds its own and no other.
pub const STATE: &str = "/state";

/// One directory a file-server role serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RoleDir {
    /// What every program's namespace calls it, after `fs:`: `/home`.
    pub dir: &'static str,
    /// Where it is on the role's volume, which a grant on it names
    /// (`toyos::fs::Grant`): DATA keeps its directories side by side, and
    /// SYSTEM, LOG and BOOT are each served whole.
    pub root: &'static str,
    /// Whether a grant on it may change it: SYSTEM's ROOT is the signed
    /// image, which nothing does.
    pub write: bool,
}

const fn dir(dir: &'static str, root: &'static str) -> RoleDir {
    RoleDir { dir, root, write: true }
}

/// The role serving ROOT, which the supervisor starts before it reads a file.
pub const SYSTEM: &str = "system";

/// The file-server roles, and the directories each serves: one port per role,
/// and one grant on it per directory.
pub const ROLES: [(&str, &[RoleDir]); 4] = [
    (SYSTEM, &[RoleDir { dir: "/system", root: "", write: false }]),
    ("data", &[dir("/apps", "apps"), dir("/config", "config"), dir("/home", "home"), dir("/state", "state")]),
    ("log", &[dir("/log", "")]),
    ("boot", &[dir("/boot", "")]),
];

/// The directories `role` serves, or `None` for a name that is no role.
pub fn role_dirs(role: &str) -> Option<&'static [RoleDir]> {
    ROLES.iter().find(|(name, _)| *name == role).map(|(_, dirs)| *dirs)
}

/// One directory capability in a program's view: the grant the supervisor
/// mints on `role`'s port (`toyos::fs::Grant`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    pub role: &'static str,
    /// What the program's namespace calls it, after `fs:`.
    pub dir: String,
    /// Where it is on the role's volume.
    pub root: String,
    /// Whether a request that changes what it holds is served.
    pub write: bool,
}

/// Every directory every role serves, each read-write but `/system`: the
/// supervisor's own view.
pub fn whole_tree() -> Vec<View> {
    ROLES
        .iter()
        .flat_map(|(role, dirs)| {
            dirs.iter().map(|d| View { role, dir: d.dir.to_string(), root: d.root.to_string(), write: d.write })
        })
        .collect()
}

/// `/system`, read-only: in every view.
fn system() -> View {
    let [only] = role_dirs(SYSTEM).expect("SYSTEM is a role") else { panic!("SYSTEM serves one directory") };
    View { role: SYSTEM, dir: only.dir.to_string(), root: only.root.to_string(), write: false }
}

/// The DATA directory `dir`, beneath one DATA serves.
fn data_dir(dir: String, write: bool) -> View {
    let role = "data";
    let parent = role_dirs(role)
        .expect("DATA is a role")
        .iter()
        .find(|d| dir.strip_prefix(d.dir).is_some_and(|rest| rest.starts_with('/')))
        .unwrap_or_else(|| panic!("manifest: {dir} is beneath no directory DATA serves"));
    let root = format!("{}{}", parent.root, &dir[parent.dir.len()..]);
    View { role, dir, root, write }
}

pub use toyos_abi::handle::Rights;
pub use toyos_abi::syscall::{DeviceRequest, DeviceType};

/// The rights a `syscap` record may name.
///
/// **A short list on purpose.** Every entry is a machine-wide authority that
/// exists nowhere else, so a name added here is a decision — and a config that
/// can write a name the supervisor cannot act on is what this being the only spelling
/// prevents.
///
/// `TRANSFER` is not nameable and is always added: the supervisor endows the duplicate,
/// and endowing is a transfer, so a cap without it could not reach the program
/// the config is talking about at all.
const SYSCAP_RIGHTS: &[(&str, Rights)] = &[
    ("rt", Rights::RT),
    ("device", Rights::DEVICE),
    // Not an authority over the machine but over the *capability*: it says out
    // loud that this program hands the cap on to its own children. The test
    // estate is its one holder — one boot runs several binaries that each need
    // the keyboard, and a claim moves.
    ("dup", Rights::DUP),
    // Read the whole machine's kernel log: every record every CPU wrote, which
    // is every process's business and no process's right by default.
    //
    // **Two bits under one name, because it is one job.** `LOG` is what
    // `SYS_LOG_READ` answers to, and `WAIT` is what lets the same capability be
    // named in an io_uring `POLL_ADD` on the log's readiness source. The call
    // never blocks by design, so a holder that may read and may not park is a
    // holder that has to spin — a name that looks complete and traps the one
    // program whose whole loop is read-then-park.
    ("logread", Rights::LOG.union(Rights::WAIT)),
    // Power the machine off. The largest authority on the list — it ends every
    // process there is, including the ones that hold every other right here — and the
    // last but one to have been free: `SYS_SHUTDOWN` took no handle at all, so
    // a program endowed exactly one connector could halt the machine with it.
    ("power", Rights::POWER),
    // Read the roster of every process in the machine: `SYS_SYSINFO`'s
    // per-thread entries, each carrying a pid, a size, a CPU time and a name.
    // The machine header the same call answers first is ambient, so `free` and
    // every daemon that sizes itself off total memory name nothing here — this
    // is the census alone, and `/system/bin/ps` is what it is for.
    ("roster", Rights::ROSTER),
    // Read what the machine is made of and who holds each part of it:
    // `SYS_DEVICE_INVENTORY`'s records. A census of the hardware and of which
    // program drives what, and `/system/bin/inspect` is what it is for.
    ("inventory", Rights::INVENTORY),
    // Read the machine's counters: `SYS_COUNTERS`, each CPU's clock reading
    // and how often firmware took it over.
    ("counters", Rights::COUNTERS),
    // Read what times every other program: each CPU's frequency, its busy
    // fraction and its wake-ups. An admin tool's, and no applet's.
    ("trace", Rights::TRACE),
    // Start programs as independent jobs, each with its own share of the CPU:
    // a launcher's. Every other child joins its parent's job, so this is what
    // lets a program gain aggregate CPU by starting many.
    ("jobs", Rights::JOBS),
    // Hold every idle CPU to waking within a bound: what an audio or other
    // deadline-bound service holds while it has a deadline, paid for in the
    // whole machine's power.
    ("wake-latency", Rights::WAKE_LATENCY),
];

/// The whole right set a program's `syscap` list asks for.
pub fn syscap_rights(names: &[String]) -> Result<Rights, String> {
    let mut rights = Rights::TRANSFER;
    for name in names {
        let (_, right) = SYSCAP_RIGHTS
            .iter()
            .find(|(n, _)| n == name)
            .ok_or_else(|| format!("`{name}` is not a syscap right"))?;
        rights = rights.union(*right);
    }
    Ok(rights)
}

/// What a row's process runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// The executable at this path: a file of `/system`, or an installed
    /// package's.
    Path(String),
    /// The file server's boot image, which the kernel hands the supervisor
    /// (`toyos_abi::boot::BootImage`): what serves `/system` runs from no file.
    BootFileServer,
}

impl Default for Source {
    fn default() -> Self {
        Self::Path(String::new())
    }
}

impl core::fmt::Display for Source {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Path(path) => f.write_str(path),
            Self::BootFileServer => f.write_str("the file server's boot image"),
        }
    }
}

impl Source {
    /// The executable's path; `None` for a boot image.
    pub fn path(&self) -> Option<&str> {
        match self {
            Self::Path(path) => Some(path),
            Self::BootFileServer => None,
        }
    }
}

#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct Program {
    pub name: String,
    pub source: Source,
    pub args: Vec<String>,
    /// Machine-wide ports the supervisor creates and endows the **acceptor** of.
    pub serves: Vec<String>,
    /// Ports this program makes for itself, once per instance. The supervisor creates
    /// nothing and holds nothing for these.
    pub provides: Vec<String>,
    /// Names in this program's namespace, each a connector.
    pub receives: Vec<String>,
    /// Claims the supervisor mints for the one process of this row ([`device::Entry`]).
    pub devices: Vec<String>,
    /// PCI matchers ([`device::PciMatch`]): the supervisor starts one process
    /// of this row per function one names and the kernel does not drive,
    /// holding that function's claim. Such a row serves nothing and is never
    /// in [`Manifest::start`]; it reaches its consumer through `receives`.
    pub drives: Vec<String>,
    /// Rights on the `SysCap` duplicate the supervisor endows this program, by the names
    /// [`syscap_rights`] takes. Empty for all but a handful: nothing else in
    /// the system may enter the RT band, mint a device claim, read the machine
    /// log, list every process in the machine, or power the machine off.
    pub syscap: Vec<String>,
    /// The machine's idle slot: the slot table's partition and the idle
    /// slot's FAT volume and ROOT (`toyos_update::slots`). The
    /// authority to write the next image and nothing else: the slot a boot
    /// runs is never among them. One program holds it — `build/build.rs` gates
    /// which.
    pub slots: bool,
    /// Disk authorizations the supervisor mints at its start, by
    /// [`authorize::Authorization::word`]: the signed image's word alone, and
    /// `build/build.rs` gates which rows.
    pub authorize: Vec<String>,
    /// A system service: its `HOME` is its own [`STATE`] directory rather
    /// than the session user's home, so what it keeps is machine state and no
    /// user's. It keeps state ([`Self::state_dir`]) whether or not
    /// [`Self::state`] says so.
    pub service: bool,
    /// It keeps machine state of its own in [`STATE`]`/<name>` beside the
    /// session's `HOME`: pkg's accepted repository metadata, say.
    pub state: bool,
    /// The file-server roles this row serves, one process each: the supervisor starts
    /// the binary once per role, with the role as its argument and the
    /// acceptor of the role's port ([`role_dirs`] are the grants on it).
    pub roles: Vec<String>,
    /// The supervisor starts it again when it ends other than by exit 0, on the
    /// same ports or function, within the supervisor's restart budget.
    pub restart: bool,
    /// Started as a pager (`toyos_abi::handle::Rights::PAGER`): it may make
    /// the paged objects its readers wait on. The signed image's row alone
    /// confers it; no [`syscap_rights`] name and no package row reaches it.
    pub pager: bool,
    /// The folder of the session's home a package's launch was granted
    /// ([`grants::decide`]): set by the supervisor on the row it synthesizes
    /// for that launch, and never on a declared row.
    pub folder: Option<grants::Folder>,
    /// The rows it may start through the supervisor's launcher: program keys,
    /// and [`launch::APPS`] for any installed package. Non-empty is what
    /// endows it a launcher at all.
    pub starts: Vec<String>,
    /// A login authority: the supervisor opens login sessions for it, the
    /// screen's row's at its start and any on `toyos_session::LOGIN_PORT`.
    /// ([`launch`]).
    pub login: bool,
    /// Bytes held back for its job however short every other job runs: the
    /// protected capacity of a control path the machine has to keep
    /// answering under exhaustion, and zero for every other row. Only the
    /// signed image declares one; an installed package's row never has one.
    pub memory_floor: u64,
}

impl Program {
    /// It replaces what the machine runs — the swap port's holder, or the idle
    /// slot's — so it starts only in a login session.
    pub fn login_only(&self) -> bool {
        self.slots || self.receives.iter().any(|r| r == SWAP_PORT)
    }

    /// The installed package this row launches: a row [`Manifest::app_row`]
    /// made.
    pub fn package(&self) -> Option<&str> {
        package::package_of(self.source.path()?)
    }

    /// Its own directory of machine state, which its view holds and no other
    /// row's does: a service's, or a row's that declares [`Self::state`].
    pub fn state_dir(&self) -> Option<String> {
        (self.service || self.state).then(|| format!("{STATE}/{}", self.name))
    }

    /// The `HOME` the supervisor starts this row with. A location grants nothing: what
    /// the program can reach is its view's business, never this string's.
    pub fn home(&self) -> String {
        match (self.service, self.package()) {
            (true, _) => self.state_dir().expect("a service keeps state"),
            (false, Some(name)) => app_home(name),
            (false, None) => session_home(),
        }
    }

    /// The directories this row's program is endowed. **An installed package
    /// sees `/system` and its own directory read-only, its own folder of the
    /// home read-write and the folder its launch was granted, and nothing else
    /// any role serves**: no other package, no other part of the home, no
    /// `/config`, `/state`, `/log` or `/boot`.
    /// **Every other row sees the whole tree but [`STATE`], and of that only its
    /// own [`Self::state_dir`]**: no row's view holds another row's, the
    /// supervisor's included. The rest stays whole until each row
    /// declares its own view (`issues/every-program-sees-only-the-files-it-was-given.md`, stage 2).
    pub fn view(&self) -> Vec<View> {
        match self.package() {
            Some(name) => {
                let granted = self.folder.iter().map(|f| data_dir(f.path.clone(), f.access == grants::Access::ReadWrite));
                [system(), data_dir(package::Package::dir(name), false), data_dir(app_home(name), true)]
                    .into_iter()
                    .chain(granted)
                    .collect()
            }
            None => whole_tree()
                .into_iter()
                .filter(|view| view.dir != STATE)
                .chain(self.state_dir().map(|dir| data_dir(dir, true)))
                .collect(),
        }
    }
}

#[derive(Default, Debug, PartialEq, Eq)]
pub struct Manifest {
    /// Sorted by name, which is what makes [`render`] byte-for-byte
    /// deterministic.
    pub programs: Vec<Program>,
    /// Names the supervisor serves itself. The supervisor is in every image and is no `[programs]`
    /// key, so these have no declaration to come from.
    pub supervisor_serves: Vec<String>,
    /// The connectors every program launched from `/apps` is given beside its
    /// view ([`Program::view`]), and nothing else. `/apps` is writable to
    /// the installer, so this row is the image's rather than the package's —
    /// which is why a device class and a `syscap` right have no spelling on
    /// the package side at all.
    pub apps: Vec<String>,
    /// The most of a folder of the session's home a package may be granted
    /// (`[apps] folder`), and, until signed package entries carry their own
    /// request, what every package asks for: `None` grants none.
    pub folder: Option<grants::Access>,
    /// Program names, in the order `[boot] start` gave them — which orders
    /// nothing, because every port exists before any server runs.
    pub start: Vec<String>,
    /// [`Program::memory_floor`] for the supervisor's own job.
    pub supervisor_floor: u64,
}

impl Manifest {
    pub fn program(&self, name: &str) -> Option<&Program> {
        self.programs.iter().find(|p| p.name == name)
    }

    /// The row a launch of an installed package is built from: synthesized,
    /// because a package has no `[programs]` key to hold one. **A package
    /// named after a row the image declares is refused** ([`row_named`]): its
    /// folder of the home would be that row's.
    pub fn app_row(&self, name: &str, program: &str) -> Result<Program, String> {
        if self.program(name).is_some() {
            return Err(row_named(name));
        }
        Ok(Program {
            name: name.to_string(),
            source: Source::Path(program.to_string()),
            receives: self.apps.clone(),
            ..Program::default()
        })
    }

    /// Every `serves` name in the whole manifest, not only the ones [`start`]
    /// names: the filepicker is launched by the compositor, and an editor
    /// holding its connector must be able to ask for a file before the picker
    /// has run an instruction.
    ///
    /// [`start`]: Self::start
    pub fn served_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self
            .programs
            .iter()
            .flat_map(|p| p.serves.iter().map(String::as_str))
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    }
}

/// Why a manifest could not be rendered.
///
/// The build system is the only caller and a failure stops the image: a name
/// with a space in it would parse back as a different record, so it is refused
/// where it is written rather than discovered where it is read.
#[derive(Debug, PartialEq, Eq)]
pub enum RenderError {
    NameTooLong(String),
    /// A field whose bytes would not survive the round trip.
    Unrepresentable { program: String, field: &'static str, value: String },
    /// A row that serves a machine-wide port and is not marked a service, so
    /// the supervisor would start it in the session user's home. A service that serves
    /// nothing (`sshserver`) cannot be told from its row, and is marked by hand.
    ServesWithoutService(String),
    /// A `roles` entry naming no file-server role.
    NoSuchRole { program: String, role: String },
    /// An `authorize` entry naming no [`authorize::Authorization`].
    NoSuchAuthorization { program: String, word: String },
    /// A `devices` entry that names no device, or a `drives` entry that is
    /// no PCI matcher.
    NoSuchDevice { program: String, entry: String },
    /// Two entries one device can answer, by row and entry: the second claim
    /// would be refused at boot, or two rows would race for one function.
    Overlap { first: (String, String), second: (String, String) },
    /// A `drives` row that also has what one process per function cannot
    /// carry: `serves`, `roles` or `devices`, or a place in `[boot] start` or
    /// another row's `starts`, either of which would start it holding no
    /// function.
    Drives { program: String, also: &'static str },
}

pub fn render(manifest: &Manifest) -> Result<Vec<u8>, RenderError> {
    claims_apart(manifest)?;
    let mut out = String::new();
    for program in &manifest.programs {
        if program.name.len() > MAX_PROGRAM_NAME {
            return Err(RenderError::NameTooLong(program.name.clone()));
        }
        check(&program.name, "name", &program.name)?;
        if !program.serves.is_empty() && !program.service {
            return Err(RenderError::ServesWithoutService(program.name.clone()));
        }
        match &program.source {
            Source::Path(path) => {
                check(&program.name, "path", path)?;
                out.push_str(&format!("program {} {path}\n", program.name));
            }
            Source::BootFileServer => out.push_str(&format!("boot-program {}\n", program.name)),
        }
        for arg in &program.args {
            reject_newline(&program.name, "args", arg)?;
            out.push_str(&format!("arg {arg}\n"));
        }
        for (field, values) in [
            ("serves", &program.serves),
            ("provides", &program.provides),
            ("receives", &program.receives),
            ("devices", &program.devices),
            ("drives", &program.drives),
            ("syscap", &program.syscap),
        ] {
            let word = match field {
                "serves" => "serve",
                "provides" => "provide",
                "receives" => "receive",
                "devices" => "device",
                "drives" => "drive",
                _ => "syscap",
            };
            for value in values {
                check(&program.name, field, value)?;
                out.push_str(&format!("{word} {value}\n"));
            }
        }
        if program.slots {
            out.push_str("slots\n");
        }
        for word in &program.authorize {
            if authorize::Authorization::parse(word).is_none() {
                return Err(RenderError::NoSuchAuthorization { program: program.name.clone(), word: word.clone() });
            }
            out.push_str(&format!("authorize {word}\n"));
        }
        if program.service {
            out.push_str("service\n");
        }
        if program.state {
            out.push_str("state\n");
        }
        for role in &program.roles {
            if role_dirs(role).is_none() {
                return Err(RenderError::NoSuchRole { program: program.name.clone(), role: role.clone() });
            }
            out.push_str(&format!("role {role}\n"));
        }
        if program.restart {
            out.push_str("restart\n");
        }
        if program.pager {
            out.push_str("pager\n");
        }
        for key in &program.starts {
            check(&program.name, "starts", key)?;
            out.push_str(&format!("starts {key}\n"));
        }
        if program.login {
            out.push_str("login\n");
        }
        if program.memory_floor > 0 {
            out.push_str(&format!("memory-floor {}\n", program.memory_floor));
        }
    }
    if manifest.supervisor_floor > 0 {
        out.push_str(&format!("supervisor-floor {}\n", manifest.supervisor_floor));
    }
    for name in &manifest.supervisor_serves {
        check("supervisor", "supervisor_serves", name)?;
        out.push_str(&format!("supervisor-serve {name}\n"));
    }
    for name in &manifest.apps {
        check("supervisor", "apps", name)?;
        out.push_str(&format!("app-receive {name}\n"));
    }
    if let Some(access) = manifest.folder {
        out.push_str(&format!("app-folder {access}\n"));
    }
    for name in &manifest.start {
        check("supervisor", "start", name)?;
        out.push_str(&format!("start {name}\n"));
    }
    Ok(out.into_bytes())
}

/// Every entry names a device, a `drives` row is one process per function and
/// nothing else, and no device can answer two entries — but two different
/// `drives` entries of one row, which start one process on a function either
/// way, under the first that matches it.
fn claims_apart(manifest: &Manifest) -> Result<(), RenderError> {
    let mut seen: Vec<(&str, &str, device::Entry, bool)> = Vec::new();
    for program in &manifest.programs {
        let refused = |entry: &String| RenderError::NoSuchDevice { program: program.name.clone(), entry: entry.clone() };
        if !program.drives.is_empty() {
            let also = [
                (!program.serves.is_empty(), "serves"),
                (!program.roles.is_empty(), "roles"),
                (!program.devices.is_empty(), "devices"),
                (manifest.start.contains(&program.name), "[boot] start"),
                (manifest.programs.iter().any(|row| row.starts.contains(&program.name)), "starts"),
            ];
            if let Some((_, also)) = also.into_iter().find(|(has, _)| *has) {
                return Err(RenderError::Drives { program: program.name.clone(), also });
            }
        }
        let devices =
            program.devices.iter().map(|e| device::Entry::parse(e).ok_or_else(|| refused(e)).map(|p| (e, p, false)));
        let drives = program.drives.iter().map(|e| match device::Entry::parse(e) {
            Some(parsed @ device::Entry::Pci(_)) => Ok((e, parsed, true)),
            _ => Err(refused(e)),
        });
        for entry in devices.chain(drives) {
            let (text, parsed, driven) = entry?;
            let apart = |row: &str, other: &str, earlier_driven: bool| {
                driven && earlier_driven && row == program.name && other != text.as_str()
            };
            if let Some((row, other, ..)) =
                seen.iter().find(|(row, other, earlier, was)| earlier.overlaps(&parsed) && !apart(row, other, *was))
            {
                return Err(RenderError::Overlap {
                    first: (row.to_string(), other.to_string()),
                    second: (program.name.clone(), text.clone()),
                });
            }
            seen.push((&program.name, text, parsed, driven));
        }
    }
    Ok(())
}

/// A field that becomes a whole record: no whitespace at all, because the
/// parser splits the record word off at the first space.
fn check(program: &str, field: &'static str, value: &str) -> Result<(), RenderError> {
    if value.is_empty() || value.contains(char::is_whitespace) {
        return Err(RenderError::Unrepresentable {
            program: program.to_string(),
            field,
            value: value.to_string(),
        });
    }
    Ok(())
}

/// An argument is the rest of its line, so it may hold spaces and may not hold
/// a newline.
fn reject_newline(program: &str, field: &'static str, value: &str) -> Result<(), RenderError> {
    if value.contains('\n') {
        return Err(RenderError::Unrepresentable {
            program: program.to_string(),
            field,
            value: value.to_string(),
        });
    }
    Ok(())
}

/// A record's byte count, which only [`render`] writes.
fn bytes(line: &str, rest: &str) -> u64 {
    rest.parse().unwrap_or_else(|_| panic!("manifest: a byte count that is none: {line}"))
}

/// Read back what [`render`] wrote.
///
/// Panics on a line neither half can produce. This file is the build system's
/// own output travelling in the image beside the binary that reads it, so a
/// malformed record is a bug in the pair rather than untrusted input.
pub fn parse(text: &str) -> Manifest {
    let mut manifest = Manifest::default();
    for line in text.lines() {
        let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
        match word {
            "program" => {
                let (name, path) = rest
                    .split_once(' ')
                    .unwrap_or_else(|| panic!("manifest: `program` without a path: {line}"));
                assert!(
                    name.len() <= MAX_PROGRAM_NAME,
                    "manifest: program name longer than {MAX_PROGRAM_NAME}: {name}"
                );
                manifest.programs.push(Program {
                    name: name.to_string(),
                    source: Source::Path(path.to_string()),
                    ..Program::default()
                });
            }
            "boot-program" => {
                assert!(
                    rest.len() <= MAX_PROGRAM_NAME,
                    "manifest: program name longer than {MAX_PROGRAM_NAME}: {rest}"
                );
                manifest.programs.push(Program {
                    name: rest.to_string(),
                    source: Source::BootFileServer,
                    ..Program::default()
                });
            }
            "supervisor-serve" => manifest.supervisor_serves.push(rest.to_string()),
            "app-receive" => manifest.apps.push(rest.to_string()),
            "app-folder" => {
                let access = grants::Access::parse(rest).unwrap_or_else(|| panic!("manifest: `{rest}` is no access"));
                assert!(manifest.folder.replace(access).is_none(), "manifest: `app-folder` twice");
            }
            "start" => manifest.start.push(rest.to_string()),
            "supervisor-floor" => manifest.supervisor_floor = bytes(line, rest),
            "" => {}
            _ => {
                let program = manifest
                    .programs
                    .last_mut()
                    .unwrap_or_else(|| panic!("manifest: `{word}` before any program"));
                match word {
                    "arg" => program.args.push(rest.to_string()),
                    "serve" => program.serves.push(rest.to_string()),
                    "provide" => program.provides.push(rest.to_string()),
                    "receive" => program.receives.push(rest.to_string()),
                    "device" => program.devices.push(rest.to_string()),
                    "drive" => program.drives.push(rest.to_string()),
                    "syscap" => program.syscap.push(rest.to_string()),
                    "slots" if rest.is_empty() => program.slots = true,
                    "authorize" if authorize::Authorization::parse(rest).is_some() => program.authorize.push(rest.to_string()),
                    "service" => program.service = true,
                    "state" if rest.is_empty() => program.state = true,
                    "role" => program.roles.push(rest.to_string()),
                    "restart" if rest.is_empty() => program.restart = true,
                    "pager" if rest.is_empty() => program.pager = true,
                    "starts" => program.starts.push(rest.to_string()),
                    "login" if rest.is_empty() => program.login = true,
                    "memory-floor" => program.memory_floor = bytes(line, rest),
                    other => panic!("manifest: unknown record `{other}`"),
                }
            }
        }
    }
    manifest
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Manifest {
        Manifest {
            programs: vec![
                Program {
                    name: "compositor".into(),
                    source: Source::Path("/system/bin/compositor".into()),
                    serves: vec!["compositor".into()],
                    receives: vec!["soundserver".into()],
                    devices: vec!["framebuffer".into(), "acpi".into()],
                    service: true,
                    starts: vec!["terminal".into(), "/apps".into()],
                    login: true,
                    ..Program::default()
                },
                Program {
                    name: "soundserver".into(),
                    source: Source::Path("/system/bin/soundserver".into()),
                    serves: vec!["soundserver".into()],
                    devices: vec!["pci:04:01:00/1af4:1059".into()],
                    syscap: vec!["rt".into()],
                    service: true,
                    ..Program::default()
                },
                Program {
                    name: "update".into(),
                    source: Source::Path("/system/bin/update".into()),
                    slots: true,
                    state: true,
                    authorize: vec!["tables".into(), "designate:data".into()],
                    ..Program::default()
                },
                Program {
                    name: "fileserver".into(),
                    source: Source::BootFileServer,
                    receives: vec!["block".into()],
                    roles: vec!["data".into(), "log".into()],
                    restart: true,
                    pager: true,
                    ..Program::default()
                },
                Program {
                    name: "vdagent".into(),
                    source: Source::Path("/system/bin/vdagent".into()),
                    receives: vec!["clipboard".into()],
                    drives: vec!["pci:07:80:00/1af4:1043".into()],
                    memory_floor: 16 << 20,
                    ..Program::default()
                },
                Program {
                    name: "terminal".into(),
                    source: Source::Path("/system/bin/terminal".into()),
                    args: vec!["--login shell".into()],
                    provides: vec!["surface".into()],
                    receives: vec!["compositor".into()],
                    ..Program::default()
                },
            ],
            supervisor_serves: vec!["swap".into()],
            apps: vec!["compositor".into(), "soundserver".into()],
            folder: Some(grants::Access::ReadWrite),
            start: vec!["compositor".into(), "soundserver".into()],
            supervisor_floor: 48 << 20,
        }
    }

    /// **An installed package holds connectors and nothing else**, and by the
    /// row's construction rather than by a check.
    #[test]
    fn a_package_row_is_connectors_and_nothing_else() {
        let row = sample().app_row("gbae", "/apps/gbae/gbae").unwrap();
        assert_eq!(row.receives, ["compositor", "soundserver"]);
        assert!(row.devices.is_empty());
        assert!(row.syscap.is_empty());
        assert!(row.serves.is_empty());
        assert!(row.provides.is_empty());
        assert_eq!(row.memory_floor, 0, "a package's row held memory back");
        assert!(!row.pager, "a package's row is a pager");
        assert_eq!(row.source.path(), Some("/apps/gbae/gbae"));
    }

    /// The one property both halves depend on, and the reason they live here.
    #[test]
    fn what_the_build_writes_is_what_the_supervisor_reads() {
        let manifest = sample();
        let rendered = render(&manifest).expect("render");
        assert_eq!(parse(std::str::from_utf8(&rendered).unwrap()), manifest);
    }

    /// A row that serves a machine-wide port is a service, and one not marked
    /// so is refused rather than started in the session user's home.
    #[test]
    fn a_row_that_serves_a_port_and_is_no_service_is_refused() {
        let mut manifest = sample();
        manifest.programs[0].service = false;
        assert_eq!(
            render(&manifest),
            Err(RenderError::ServesWithoutService("compositor".into()))
        );
    }

    /// A service keeps machine state under its own name, everything else is
    /// the session's; and a package's synthesized row is never a service.
    #[test]
    fn a_service_s_home_is_its_state_and_every_other_row_s_the_session_s() {
        let m = sample();
        assert_eq!(m.program("soundserver").unwrap().home(), "/state/soundserver");
        assert_eq!(m.program("terminal").unwrap().home(), "/home/toy");
        let m = parse("program sshserver /system/bin/sshserver\nservice\nprogram shell /system/bin/shell\n");
        assert!(m.program("sshserver").unwrap().service);
        assert!(!m.program("shell").unwrap().service);
        // The shell keeps its history under the session's home, in its own
        // `Apps/shell` (`OWN_FOLDER` in `system/shell`).
        assert_eq!(m.program("shell").unwrap().home(), "/home/toy");
    }

    /// **An installed package's `HOME` is its own folder** of the session
    /// user's home, the layout's `/home/<user>/Apps/<name>`.
    #[test]
    fn a_package_s_home_is_its_own_folder() {
        let row = sample().app_row("gbae", "/apps/gbae/gbae").unwrap();
        assert_eq!(row.package(), Some("gbae"));
        assert_eq!(row.home(), "/home/toy/Apps/gbae");
        assert_eq!(sample().program("compositor").unwrap().package(), None);
    }

    /// **A package named after a declared row is refused**, by name: the
    /// shell keeps its history in `Apps/shell`, which would be the package's
    /// folder.
    #[test]
    fn a_package_named_after_a_declared_row_is_refused() {
        let m = parse("program shell /system/bin/shell\n");
        assert_eq!(m.app_row("shell", "/apps/shell/shell"), Err(row_named("shell")));
        assert!(row_named("shell").contains("/home/toy/Apps/shell"));
        let s = sample();
        for row in &s.programs {
            assert_eq!(s.app_row(&row.name, &format!("/apps/{0}/{0}", row.name)), Err(row_named(&row.name)));
        }
        assert!(m.app_row("gbae", "/apps/gbae/gbae").is_ok());
    }

    fn views(row: &Program) -> Vec<(&'static str, String, String, bool)> {
        row.view().into_iter().map(|v| (v.role, v.dir, v.root, v.write)).collect()
    }

    /// `/system`, read-only, as every view holds it.
    fn system() -> (&'static str, String, String, bool) {
        (SYSTEM, "/system".into(), String::new(), false)
    }

    /// **An installed package sees `/system` and its own directory read-only
    /// and its own folder read-write, and nothing else any role serves.**
    /// Spelled out whole, so a fourth directory, a wider root or a writable
    /// package is red.
    #[test]
    fn a_package_s_view_is_its_own_directory_read_only_and_its_own_folder() {
        let row = sample().app_row("gbae", "/apps/gbae/gbae").unwrap();
        assert_eq!(
            views(&row),
            [
                system(),
                ("data", "/apps/gbae".into(), "apps/gbae".into(), false),
                ("data", "/home/toy/Apps/gbae".into(), "home/toy/Apps/gbae".into(), true),
            ]
        );
        // The longest name a package has is still beneath its own directories.
        let longest = "n".repeat(MAX_PROGRAM_NAME);
        let row = sample().app_row(&longest, &format!("/apps/{longest}/{longest}")).unwrap();
        assert_eq!(
            views(&row).into_iter().skip(1).map(|(_, _, root, write)| (root, write)).collect::<Vec<_>>(),
            [(format!("apps/{longest}"), false), (format!("home/toy/Apps/{longest}"), true)]
        );
    }

    /// **A package's launch granted a folder sees it too, at the access it
    /// was granted**, and nothing else is added. Spelled out whole.
    #[test]
    fn a_package_granted_a_folder_sees_it_beside_its_own() {
        for (access, write) in [(grants::Access::ReadWrite, true), (grants::Access::ReadOnly, false)] {
            let mut row = sample().app_row("gbae", "/apps/gbae/gbae").unwrap();
            row.folder = Some(grants::Folder { path: "/home/toy/Games".into(), access });
            assert_eq!(
                views(&row),
                [
                    system(),
                    ("data", "/apps/gbae".into(), "apps/gbae".into(), false),
                    ("data", "/home/toy/Apps/gbae".into(), "home/toy/Apps/gbae".into(), true),
                    ("data", "/home/toy/Games".into(), "home/toy/Games".into(), write),
                ]
            );
        }
    }

    /// **Every row the image declares sees the whole tree read-write but
    /// `/system` and `/state`, and of `/state` only its own directory**: a
    /// service's and a row's that declares state, none for any other. The
    /// installer and the shell are alike: neither has authority over `/apps`
    /// the other lacks, nobody writes the signed image, and nobody reads or
    /// rewrites another program's machine state, the supervisor's included.
    #[test]
    fn a_declared_row_sees_the_whole_tree_but_state_and_of_state_its_own() {
        let m = parse("program pkg /system/bin/pkg\nstate\nprogram shell /system/bin/shell\n");
        let whole_but_state = |own: Option<&str>| -> Vec<_> {
            std::iter::once(system())
                .chain(
                    ["/apps", "/config", "/home", "/log", "/boot"]
                        .iter()
                        .zip(["apps", "config", "home", "", ""])
                        .zip(["data", "data", "data", "log", "boot"])
                        .map(|((dir, root), role)| (role, dir.to_string(), root.to_string(), true)),
                )
                .chain(own.map(|name| ("data", format!("/state/{name}"), format!("state/{name}"), true)))
                .collect()
        };
        let s = sample();
        for (row, own) in [
            (m.program("shell").unwrap(), None),
            (m.program("pkg").unwrap(), Some("pkg")),
            (s.program("compositor").unwrap(), Some("compositor")),
        ] {
            assert_eq!(views(row), whole_but_state(own), "{}", row.name);
        }
        // A row that keeps state keeps its session `HOME`; a service's is its state.
        assert_eq!(m.program("pkg").unwrap().home(), session_home());
        assert_eq!(s.program("compositor").unwrap().home(), "/state/compositor");
        assert_eq!(m.program("shell").unwrap().state_dir(), None);
    }

    #[test]
    fn the_same_manifest_renders_to_the_same_bytes() {
        assert_eq!(render(&sample()), render(&sample()));
    }

    #[test]
    fn records_attach_to_the_program_above_them() {
        let m = parse(
            "program soundserver /system/bin/soundserver\nserve soundserver\nsyscap rt\n\
             program toybox /system/bin/toybox\narg pwd\nreceive compositor\n\
             supervisor-serve launcher\nstart soundserver\n",
        );
        assert_eq!(m.program("soundserver").unwrap().syscap, ["rt"]);
        assert!(m.program("toybox").unwrap().syscap.is_empty());
        assert_eq!(m.program("toybox").unwrap().args, ["pwd"]);
        assert_eq!(m.served_names(), ["soundserver"]);
    }

    /// A name with a space in it parses back as a different record, so it is
    /// refused where it is written.
    #[test]
    fn a_name_that_would_not_survive_the_round_trip_is_refused() {
        let mut bad = sample();
        bad.programs[0].serves = vec!["two words".into()];
        assert!(matches!(render(&bad), Err(RenderError::Unrepresentable { .. })));

        let mut long = sample();
        long.programs[0].name = "x".repeat(MAX_PROGRAM_NAME + 1);
        assert!(matches!(render(&long), Err(RenderError::NameTooLong(_))));

        let mut newline = sample();
        newline.programs[2].args = vec!["a\nb".into()];
        assert!(matches!(render(&newline), Err(RenderError::Unrepresentable { .. })));
    }

    /// `TRANSFER` is in every set and is nameable in none: the supervisor endows the
    /// duplicate, so a set without it names a capability that cannot reach the
    /// program the config is about.
    #[test]
    fn a_syscap_set_always_carries_transfer_and_never_an_invented_right() {
        assert_eq!(syscap_rights(&[]).unwrap(), Rights::TRANSFER);
        assert_eq!(
            syscap_rights(&["device".into(), "dup".into()]).unwrap(),
            Rights::TRANSFER.union(Rights::DEVICE).union(Rights::DUP)
        );
        assert!(syscap_rights(&["transfer".into()]).is_err());
        assert!(syscap_rights(&["root".into()]).is_err());
    }

    /// **The one name that is two bits**, asserted because the pair is the
    /// decision and not an accident of how it was written: a log reader that
    /// may read and may not park has to spin, `SYS_LOG_READ` never blocking by
    /// design.
    #[test]
    fn logread_carries_both_halves_of_reading_a_stream_that_never_blocks() {
        assert_eq!(
            syscap_rights(&["logread".into()]).unwrap(),
            Rights::TRANSFER.union(Rights::LOG).union(Rights::WAIT)
        );
        // And it is not the RT band's, nor a device claim's, however it is
        // spelled.
        assert!(syscap_rights(&["log".into()]).is_err());
    }

    /// **The census is one bit and the log is another**, asserted because the
    /// two are the same shape — a machine-wide reading no program gets by
    /// default — and a config that named one meaning the other would build an
    /// image whose `ps` works and whose `logkeeper` writes nothing, or the reverse.
    ///
    /// `WAIT` is deliberately absent: `SYS_SYSINFO` answers where it stands and
    /// there is nothing to park on, so a roster holder needs no readiness
    /// source the way `logread` does.
    #[test]
    fn the_process_roster_is_its_own_name_and_its_own_bit() {
        assert_eq!(
            syscap_rights(&["roster".into()]).unwrap(),
            Rights::TRANSFER.union(Rights::ROSTER)
        );
        assert!(!syscap_rights(&["roster".into()]).unwrap().contains(Rights::LOG));
        assert!(!syscap_rights(&["logread".into()]).unwrap().contains(Rights::ROSTER));
        // Not the applet's name, and not the syscall's.
        assert!(syscap_rights(&["ps".into()]).is_err());
        assert!(syscap_rights(&["sysinfo".into()]).is_err());
    }

    /// **No `syscap` name confers the pager right**: a row's `pager` is the
    /// only way to it, so a manifest's right list cannot start a pager.
    #[test]
    fn no_syscap_name_is_the_pager_right() {
        for (name, _) in SYSCAP_RIGHTS {
            let rights = syscap_rights(&[(*name).to_string()]).unwrap();
            assert!(!rights.contains(Rights::PAGER), "`{name}` carries PAGER");
        }
        let every: Vec<String> = SYSCAP_RIGHTS.iter().map(|(n, _)| (*n).to_string()).collect();
        assert!(!syscap_rights(&every).unwrap().contains(Rights::PAGER));
        assert!(syscap_rights(&["pager".into()]).is_err());
    }

    /// A file server's roles reach the supervisor as records, and a name that is no
    /// role is refused where it is written: the supervisor would start a server for
    /// directories nobody named.
    #[test]
    fn a_role_is_one_of_the_three_and_its_directories_are_fixed() {
        let m = sample();
        let fileserver = m.program("fileserver").unwrap();
        assert_eq!(fileserver.roles, ["data", "log"]);
        assert!(fileserver.restart);
        assert!(fileserver.pager);
        let dirs = |role| role_dirs(role).map(|dirs| dirs.iter().map(|d| (d.dir, d.root)).collect::<Vec<_>>());
        assert_eq!(
            dirs("data"),
            Some(vec![("/apps", "apps"), ("/config", "config"), ("/home", "home"), ("/state", "state")])
        );
        assert_eq!(dirs("boot"), Some(vec![("/boot", "")]));
        assert_eq!(role_dirs("tmp"), None);
        let mut bad = sample();
        bad.programs[3].roles = vec!["tmp".into()];
        assert_eq!(
            render(&bad),
            Err(RenderError::NoSuchRole { program: "fileserver".into(), role: "tmp".into() })
        );
    }

    /// A row's name, its `devices` and its `drives`.
    type Row<'a> = (&'a str, &'a [&'a str], &'a [&'a str]);

    fn with_rows(rows: &[Row<'_>]) -> Manifest {
        Manifest {
            programs: rows
                .iter()
                .map(|(name, devices, drives)| Program {
                    name: name.to_string(),
                    source: Source::Path(format!("/system/bin/{name}")),
                    devices: devices.iter().map(|d| d.to_string()).collect(),
                    drives: drives.iter().map(|d| d.to_string()).collect(),
                    ..Program::default()
                })
                .collect(),
            ..Manifest::default()
        }
    }

    fn overlap(first: (&str, &str), second: (&str, &str)) -> Result<Vec<u8>, RenderError> {
        Err(RenderError::Overlap {
            first: (first.0.into(), first.1.into()),
            second: (second.0.into(), second.1.into()),
        })
    }

    /// **Two entries one device can answer are refused where the image is
    /// built, naming both**: across rows, within one row's `devices`, and an
    /// entry written twice. Two different `drives` entries of one row are not:
    /// the row starts one process on the function under the first.
    #[test]
    fn two_entries_one_device_can_answer_are_refused_by_name() {
        let cases: [(&[Row<'_>], _, _); 6] = [
            (&[("a", &[], &["pci:ff:00:00", "pci:ff:00:00"])], ("a", "pci:ff:00:00"), ("a", "pci:ff:00:00")),
            (&[("a", &["framebuffer"], &[]), ("b", &["framebuffer"], &[])], ("a", "framebuffer"), ("b", "framebuffer")),
            (&[("a", &["isa:0060,0064:1,12"], &[]), ("b", &["isa:0060,0064:1,12"], &[])], ("a", "isa:0060,0064:1,12"), ("b", "isa:0060,0064:1,12")),
            (&[("a", &["pci:01:08:02/1b36:0010"], &[]), ("b", &[], &["pci:01:08:02"])], ("a", "pci:01:08:02/1b36:0010"), ("b", "pci:01:08:02")),
            (&[("a", &[], &["pci:0c:03:30"]), ("b", &[], &["pci:0c:03:30/1b36:000d"])], ("a", "pci:0c:03:30"), ("b", "pci:0c:03:30/1b36:000d")),
            (&[("a", &["pci:02:00:00", "pci:02:00:00/1af4:1041"], &[])], ("a", "pci:02:00:00"), ("a", "pci:02:00:00/1af4:1041")),
        ];
        for (rows, first, second) in cases {
            assert_eq!(render(&with_rows(rows)), overlap(first, second), "{rows:?}");
        }
        let apart = with_rows(&[
            ("a", &["framebuffer", "pci:01:08:02/1b36:0010"], &[]),
            ("b", &["acpi", "pci:01:08:02/144d:a808"], &[]),
            ("c", &[], &["pci:0c:03:30"]),
        ]);
        assert!(render(&apart).is_ok());
        let one_row = with_rows(&[("usbd", &[], &["pci:0c:03:30/1033:0194", "pci:0c:03:30@boot"])]);
        assert!(render(&one_row).is_ok());
    }

    #[test]
    fn an_entry_that_names_no_device_is_refused() {
        for (devices, drives, entry) in [
            (&["gpu"][..], &[][..], "gpu"),
            (&["pci:1af4:1041"], &[], "pci:1af4:1041"),
            (&[], &["acpi"], "acpi"),
            (&[], &["pci:01:08"], "pci:01:08"),
        ] {
            let refused = RenderError::NoSuchDevice { program: "a".into(), entry: entry.into() };
            assert_eq!(render(&with_rows(&[("a", devices, drives)])), Err(refused));
        }
    }

    /// A `drives` row is one process per function, so it serves no port, holds
    /// no claim but its function's and is started by no `[boot] start`.
    #[test]
    fn a_drives_row_is_one_process_per_function_and_nothing_else() {
        let drives = |change: fn(&mut Manifest)| {
            let mut m = with_rows(&[("a", &[], &["pci:ff:00:00/1234:11e8"])]);
            change(&mut m);
            render(&m)
        };
        let refused = |also| Err(RenderError::Drives { program: "a".into(), also });
        assert!(drives(|_| {}).is_ok());
        assert_eq!(drives(|m| m.programs[0].serves = vec!["edu".into()]), refused("serves"));
        assert_eq!(drives(|m| m.programs[0].roles = vec!["data".into()]), refused("roles"));
        assert_eq!(drives(|m| m.programs[0].devices = vec!["acpi".into()]), refused("devices"));
        assert_eq!(drives(|m| m.start = vec!["a".into()]), refused("[boot] start"));
        assert_eq!(drives(|m| m.programs[0].starts = vec!["a".into()]), refused("starts"));
    }

    #[test]
    fn a_device_class_name_is_the_abi_s() {
        assert_eq!(DeviceType::from_class_name("framebuffer"), Some(DeviceType::Framebuffer));
        assert_eq!(DeviceType::Framebuffer.class_name(), "framebuffer");
        assert_eq!(DeviceType::from_class_name("frame_buffer"), None);
    }
}
