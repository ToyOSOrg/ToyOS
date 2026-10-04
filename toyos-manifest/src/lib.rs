//! What every program in an image is allowed to hold, written by the build
//! system and read by `/system/bin/supervisor`.
//!
//! **One definition of the format, used by both halves.** `src/build.rs`
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
//! program <name> <path>     starts a program's records
//! arg <text>                argv after argv[0]
//! serve <name>              the supervisor makes one machine-wide port and endows the acceptor
//! provide <name>            this program makes its own port, once per instance
//! receive <name>            a connector in this program's namespace
//! device <class>            a claim the supervisor mints and endows
//! syscap <right>            a right on the SysCap dup the supervisor endows
//! slots                     the idle slot's partitions and the slot table, claimed by the supervisor
//! service                   a system service: its `HOME` is `/state/<name>`, not the session's
//! role <role>               a file server for `<role>`: one process of it per role
//! restart                   the supervisor starts it again when it ends
//! supervisor-serve <name>   a name the supervisor serves itself
//! start <name>              the supervisor starts this program at boot
//! app-receive <name>        a connector every program launched from /apps holds
//! starts <key>              a row this program may start through the launcher, or `/apps`
//! login                     a launch this program makes opens a login session
//! ```
//!
//! [`package`] is the other half: what an installed package says about itself,
//! which is which of its own binaries a launch starts and never what it holds.
//! [`launch`] is who may start what.

pub mod launch;
pub mod package;

/// Where ROOT carries it, without a leading slash — that volume's own
/// spelling. [`GUEST_PATH`] is what a process opens.
pub const PATH: &str = "etc/system.manifest";

/// The path `/system/bin/supervisor` opens.
pub const GUEST_PATH: &str = "/system/etc/system.manifest";

/// A program key may be this long. Policy on the primitive: the launcher
/// carries one in a message, and a longer one is refused by name rather than
/// truncated into some other program's.
pub const MAX_PROGRAM_NAME: usize = 32;

/// The port `/system/bin/supervisor` serves swaps on: its one holder replaces a
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

/// Where each system service keeps its own persistent data, one directory per
/// program key.
pub const STATE: &str = "/state";

/// The file-server roles, and the directories each serves: one capability per
/// directory, named `fs:` and the directory.
pub const ROLES: [(&str, &[&str]); 3] = [
    ("data", &["/apps", "/config", "/home", "/state"]),
    ("log", &["/log"]),
    ("boot", &["/boot"]),
];

/// The directories `role` serves, or `None` for a name that is no role.
pub fn role_dirs(role: &str) -> Option<&'static [&'static str]> {
    ROLES.iter().find(|(name, _)| *name == role).map(|(_, dirs)| *dirs)
}

/// How often a `restart` row is started again before the supervisor gives up on it: at
/// most this many ends inside [`RESTART_WINDOW_SECS`]. Past it the row's ports
/// close, and a client's next connection is answered `Gone`.
pub const RESTARTS: u32 = 3;
pub const RESTART_WINDOW_SECS: u64 = 10;

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

#[derive(Default, Debug, PartialEq, Eq)]
pub struct Program {
    pub name: String,
    pub path: String,
    pub args: Vec<String>,
    /// Machine-wide ports the supervisor creates and endows the **acceptor** of.
    pub serves: Vec<String>,
    /// Ports this program makes for itself, once per instance. The supervisor creates
    /// nothing and holds nothing for these.
    pub provides: Vec<String>,
    /// Names in this program's namespace, each a connector.
    pub receives: Vec<String>,
    pub devices: Vec<String>,
    /// Rights on the `SysCap` duplicate the supervisor endows this program, by the names
    /// [`syscap_rights`] takes. Empty for all but a handful: nothing else in
    /// the system may enter the RT band, mint a device claim, read the machine
    /// log, list every process in the machine, or power the machine off.
    pub syscap: Vec<String>,
    /// The machine's idle slot, granted as claims: the slot table's partition
    /// and the idle slot's FAT volume and ROOT, which the supervisor resolves against
    /// the ROOT the kernel holds and mints (`toyos_update::slots`). The
    /// authority to write the next image and nothing else: the slot a boot
    /// runs is never among them. One program holds it — `src/build.rs` gates
    /// which.
    pub slots: bool,
    /// A system service: its `HOME` is its own [`STATE`] directory rather
    /// than the session user's home, so what it keeps is machine state and no
    /// user's.
    pub service: bool,
    /// The file-server roles this row serves, one process each: the supervisor starts
    /// the binary once per role, with the role as its argument and the
    /// acceptors of the role's directories ([`role_dirs`]).
    pub roles: Vec<String>,
    /// The supervisor starts it again when it ends, on the same ports, for as long as
    /// it does not end faster than [`RESTARTS`] allows.
    pub restart: bool,
    /// The rows it may start through the supervisor's launcher: program keys,
    /// and [`launch::APPS`] for any installed package. Non-empty is what
    /// endows it a launcher at all.
    pub starts: Vec<String>,
    /// A launch it makes opens a login session ([`launch`]).
    pub login: bool,
}

impl Program {
    /// It replaces what the machine runs — the swap port's holder, or the idle
    /// slot's — so it starts only in a login session.
    pub fn login_only(&self) -> bool {
        self.slots || self.receives.iter().any(|r| r == SWAP_PORT)
    }

    /// The `HOME` the supervisor starts this row with. A location grants nothing: what
    /// the program can reach is its view's business, never this string's.
    pub fn home(&self) -> String {
        match self.service {
            true => format!("{STATE}/{}", self.name),
            false => session_home(),
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
    /// The namespace every program launched from `/apps` is given: connectors,
    /// and nothing else. A package directory is writable, so this row is the
    /// image's rather than the package's — which is why a device class and a
    /// `syscap` right have no spelling on the package side at all.
    pub apps: Vec<String>,
    /// Program names, in the order `[boot] start` gave them — which orders
    /// nothing, because every port exists before any server runs.
    pub start: Vec<String>,
}

impl Manifest {
    pub fn program(&self, name: &str) -> Option<&Program> {
        self.programs.iter().find(|p| p.name == name)
    }

    /// The row a launch of an installed package is built from: synthesized,
    /// because a package has no `[programs]` key to hold one.
    pub fn app_row(&self, name: &str, program: &str) -> Program {
        Program {
            name: name.to_string(),
            path: program.to_string(),
            receives: self.apps.clone(),
            ..Program::default()
        }
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
}

pub fn render(manifest: &Manifest) -> Result<Vec<u8>, RenderError> {
    let mut out = String::new();
    for program in &manifest.programs {
        if program.name.len() > MAX_PROGRAM_NAME {
            return Err(RenderError::NameTooLong(program.name.clone()));
        }
        check(&program.name, "name", &program.name)?;
        check(&program.name, "path", &program.path)?;
        if !program.serves.is_empty() && !program.service {
            return Err(RenderError::ServesWithoutService(program.name.clone()));
        }
        out.push_str(&format!("program {} {}\n", program.name, program.path));
        for arg in &program.args {
            reject_newline(&program.name, "args", arg)?;
            out.push_str(&format!("arg {arg}\n"));
        }
        for (field, values) in [
            ("serves", &program.serves),
            ("provides", &program.provides),
            ("receives", &program.receives),
            ("devices", &program.devices),
            ("syscap", &program.syscap),
        ] {
            let word = match field {
                "serves" => "serve",
                "provides" => "provide",
                "receives" => "receive",
                "devices" => "device",
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
        if program.service {
            out.push_str("service\n");
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
        for key in &program.starts {
            check(&program.name, "starts", key)?;
            out.push_str(&format!("starts {key}\n"));
        }
        if program.login {
            out.push_str("login\n");
        }
    }
    for name in &manifest.supervisor_serves {
        check("supervisor", "supervisor_serves", name)?;
        out.push_str(&format!("supervisor-serve {name}\n"));
    }
    for name in &manifest.apps {
        check("supervisor", "apps", name)?;
        out.push_str(&format!("app-receive {name}\n"));
    }
    for name in &manifest.start {
        check("supervisor", "start", name)?;
        out.push_str(&format!("start {name}\n"));
    }
    Ok(out.into_bytes())
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
                    path: path.to_string(),
                    ..Program::default()
                });
            }
            "supervisor-serve" => manifest.supervisor_serves.push(rest.to_string()),
            "app-receive" => manifest.apps.push(rest.to_string()),
            "start" => manifest.start.push(rest.to_string()),
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
                    "syscap" => program.syscap.push(rest.to_string()),
                    "slots" if rest.is_empty() => program.slots = true,
                    "service" => program.service = true,
                    "role" => program.roles.push(rest.to_string()),
                    "restart" if rest.is_empty() => program.restart = true,
                    "starts" => program.starts.push(rest.to_string()),
                    "login" if rest.is_empty() => program.login = true,
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
                    path: "/system/bin/compositor".into(),
                    serves: vec!["compositor".into()],
                    receives: vec!["soundserver".into()],
                    devices: vec!["framebuffer".into(), "keyboard".into()],
                    service: true,
                    starts: vec!["terminal".into(), "/apps".into()],
                    login: true,
                    ..Program::default()
                },
                Program {
                    name: "soundserver".into(),
                    path: "/system/bin/soundserver".into(),
                    serves: vec!["soundserver".into()],
                    devices: vec!["hda-audio".into(), "virtio-sound".into()],
                    syscap: vec!["rt".into()],
                    service: true,
                    ..Program::default()
                },
                Program {
                    name: "update".into(),
                    path: "/system/bin/update".into(),
                    slots: true,
                    ..Program::default()
                },
                Program {
                    name: "fileserver".into(),
                    path: "/system/bin/fileserver".into(),
                    receives: vec!["block".into()],
                    roles: vec!["data".into(), "log".into()],
                    restart: true,
                    ..Program::default()
                },
                Program {
                    name: "terminal".into(),
                    path: "/system/bin/terminal".into(),
                    args: vec!["--login shell".into()],
                    provides: vec!["surface".into()],
                    receives: vec!["compositor".into()],
                    ..Program::default()
                },
            ],
            supervisor_serves: vec!["swap".into()],
            apps: vec!["compositor".into(), "soundserver".into()],
            start: vec!["compositor".into(), "soundserver".into()],
        }
    }

    /// **An installed package holds connectors and nothing else**, and by the
    /// row's construction rather than by a check.
    #[test]
    fn a_package_row_is_connectors_and_nothing_else() {
        let row = sample().app_row("gbae", "/apps/gbae/gbae");
        assert_eq!(row.receives, ["compositor", "soundserver"]);
        assert!(row.devices.is_empty());
        assert!(row.syscap.is_empty());
        assert!(row.serves.is_empty());
        assert!(row.provides.is_empty());
        assert_eq!(row.path, "/apps/gbae/gbae");
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
        assert_eq!(m.app_row("gbae", "/apps/gbae/gbae").home(), "/home/toy");
        let m = parse("program sshserver /system/bin/sshserver\nservice\nprogram shell /system/bin/shell\n");
        assert!(m.program("sshserver").unwrap().service);
        assert!(!m.program("shell").unwrap().service);
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

    /// A file server's roles reach the supervisor as records, and a name that is no
    /// role is refused where it is written: the supervisor would start a server for
    /// directories nobody named.
    #[test]
    fn a_role_is_one_of_the_three_and_its_directories_are_fixed() {
        let m = sample();
        let fileserver = m.program("fileserver").unwrap();
        assert_eq!(fileserver.roles, ["data", "log"]);
        assert!(fileserver.restart);
        assert_eq!(role_dirs("data"), Some(&["/apps", "/config", "/home", "/state"][..]));
        assert_eq!(role_dirs("boot"), Some(&["/boot"][..]));
        assert_eq!(role_dirs("tmp"), None);
        let mut bad = sample();
        bad.programs[3].roles = vec!["tmp".into()];
        assert_eq!(
            render(&bad),
            Err(RenderError::NoSuchRole { program: "fileserver".into(), role: "tmp".into() })
        );
    }

    #[test]
    fn a_device_class_name_is_the_abi_s() {
        assert_eq!(DeviceType::from_class_name("hda-audio"), Some(DeviceType::HdaAudio));
        assert_eq!(DeviceType::HdaAudio.class_name(), "hda-audio");
        assert_eq!(DeviceType::from_class_name("hda_audio"), None);
    }
}
