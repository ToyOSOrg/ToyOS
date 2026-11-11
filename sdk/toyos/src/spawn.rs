//! Starting a child: a helper this process spawns, or an independent launch
//! asked of the session launcher it holds. std's `Command` and libc's
//! `posix_spawn` both start one here, so neither carries a rule of its own:
//! [`Spawn::start`] says which the spawn is, or refuses it, and what it
//! answers is the one way to start that kind.
//!
//! **A helper inherits.** It holds a duplicate of each service this process
//! was endowed ([`inherited`]), its environment as given but for its starting
//! directory, and exactly the descriptors and endowments its spawn names;
//! never this process's launcher, which only an endowment the caller names
//! hands on.
//!
//! **An independent launch runs its row.** The launcher starts the program a
//! manifest row declares, holding that row's authority and the connectors
//! provided beside it, so a launch carries no endowment, no descriptor past
//! stdio and no image of the caller's choosing. A launch this process cannot
//! make is refused, never spawned as a helper instead.
//!
//! **A helper's table is one list**, the handles it is endowed and what each
//! is called, in one order ([`Helper::spawn`]): the kernel moves the handles
//! and the names go in the child's startup block ([`crate::startup`]).

use toyos_abi::handle::TransferEntry;
use toyos_abi::syscall::{self, SpawnArgs, SyscallError, MAX_PATH, MAX_PROCESS_NAME, MAX_SPAWN_ENDOWMENTS};

use crate::endow::FD_PREFIX;

use crate::fd::Kind as FdKind;
use crate::ipc::IpcError;
use crate::launch::{self, Launch, LaunchError, Outcome, MAX_LAUNCH_EXTRAS, MAX_LAUNCH_FDS};
use crate::process::{Image, Process};
use alloc::string::String;
use alloc::vec::Vec;

use crate::{AsHandle, BorrowedHandle, Moved, OwnedHandle, RawHandle};

/// Whom a child is placed under, whose end takes it down.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Parent<'a> {
    /// This process.
    Caller,
    /// The process a handle carrying `WRITE` names.
    Place(BorrowedHandle<'a>),
    /// The launcher's owner, so that the child outlives this process. Only a
    /// launch asks it.
    Detached,
}

/// What a spawn asks for. Every handle in it is the caller's and stays the
/// caller's but the endowed and provided ones, which a spawn that starts a
/// child moves.
pub struct Spawn<'a> {
    /// The program, as the caller resolved it: what a launch names and what a
    /// helper runs when no `image` is named.
    pub program: &'a str,
    /// The program a helper runs in place of `program`.
    pub image: Option<&'a str>,
    /// The caller holds the image the child runs, a boot image or one it read
    /// itself, which only a helper takes.
    pub image_held: bool,
    /// NUL-separated, `argv[0]` first.
    pub argv: &'a [u8],
    /// `KEY=VALUE\0` repeated: the child's environment.
    pub env: &'a [u8],
    /// Absolute: the child starts only where its spawn says, which a helper
    /// states in its environment ([`crate::files::starting_in`]).
    pub cwd: &'a str,
    /// `(child descriptor, caller's handle, its kind)`, each a number once:
    /// the child starts with these and no other descriptors. The caller keeps
    /// its handles, which a spawn duplicates; a stream's or a socket's is an
    /// end the caller exported for this child (`net::Exporter`), which no
    /// other child is given. A launch carries 0 to 2 alone.
    pub fds: &'a [(u32, BorrowedHandle<'a>, FdKind)],
    /// `(label, handle)` the child is endowed, each moved, with the rights
    /// its [`Moved`] names: by a spawn that happens to the child, and closed
    /// by one refused. Only a helper takes any; one labelled as a service
    /// (`crate::endow::SVC_PREFIX`) is the child's in place of this
    /// process's of that label.
    pub endow: Vec<(&'a str, Moved)>,
    /// Which of this process's services a helper is given: a launch runs its
    /// row, and names [`Inherit::All`].
    pub inherit: Inherit<'a>,
    /// The job the child joins, a handle carrying `WRITE` this process keeps,
    /// or else this process's own.
    pub job: Option<BorrowedHandle<'a>>,
    /// `(name, connector)` a launched child is given beside its row's
    /// services, each moved as `endow`'s are.
    pub provided: Vec<(&'a str, OwnedHandle)>,
    pub parent: Parent<'a>,
    /// An independent launch through this process's launcher, where a helper
    /// is not.
    pub independent: bool,
    /// A launcher's connector this process holds, which a launch goes through
    /// in place of the one it was endowed: borrowed, and refused a helper.
    pub launcher: Option<BorrowedHandle<'a>>,
    /// A `SysCap` carrying `Rights::PAGER`, borrowed, which starts the child
    /// as a pager (`SpawnArgs::pager`): a helper's alone.
    pub pager: Option<BorrowedHandle<'a>>,
}

/// Which of the services this process was endowed a helper is given a
/// duplicate of.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Inherit<'a> {
    /// Every one.
    #[default]
    All,
    /// Those of these names (`crate::endow::connector`'s) it holds, and no other.
    Only(&'a [&'a str]),
}

/// The labels of `held`, this process's services, a helper is given: those
/// `inherit` admits, but one the spawn endows itself. The one place a
/// helper's inheritance is decided.
fn inherited<'t>(held: impl Iterator<Item = &'t str>, inherit: Inherit<'_>, endowed: &[&str]) -> Vec<&'t str> {
    held.filter(|label| !endowed.contains(label))
        .filter(|label| match inherit {
            Inherit::All => true,
            Inherit::Only(names) => crate::endow::service_of(label).is_some_and(|name| names.contains(&name)),
        })
        .collect()
}

/// The label a helper's descriptor `fd` of `kind` arrives under: a file's
/// names the first descriptor of `fds` that shares its exported connection,
/// which the child adopts once and aliases.
fn fd_label(fd: u32, handle: RawHandle, kind: FdKind, fds: &[(u32, BorrowedHandle<'_>, FdKind)]) -> String {
    match kind {
        FdKind::File => {
            let group = fds.iter().find(|(_, h, k)| *k == FdKind::File && h.raw() == handle).map_or(fd, |(first, _, _)| *first);
            alloc::format!("{FD_PREFIX}{fd}:file:{group}")
        }
        kind => alloc::format!("{FD_PREFIX}{fd}{}", kind.suffix()),
    }
}

/// Why nothing was started.
#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    /// Not permitted, for this reason.
    Permission(&'static str),
    /// The spawn names more than it can carry, or asks what its kind does
    /// not give, for this reason.
    Invalid(&'static str),
    /// It would put a stream's own handle in another process, where only an
    /// exported end of it goes (`crate::net::stream::export`).
    Stream(RawHandle),
    /// netstack made no holder of a stream the child was to be given.
    Export(crate::net::NetError),
    /// The filesystem refused to export a shared description.
    File(crate::files::Error),
    /// The launcher did not start a program it permits, or its answer was lost.
    NotStarted,
    /// The kernel refused.
    Syscall(SyscallError),
}

/// What [`Spawn::start`] found a spawn to be, holding the one way to start it.
pub enum Start<'a> {
    Helper(Helper<'a>),
    Launch(Independent<'a>),
}

/// A spawn this process makes: [`Helper::spawn`].
pub struct Helper<'a>(Spawn<'a>);

/// A spawn the launcher makes: [`Independent::launch`].
pub struct Independent<'a>(Spawn<'a>);

/// What [`kind`] judges of a spawn.
#[derive(Clone, Copy, Default, Debug)]
struct Shape {
    independent: bool,
    detached: bool,
    endowed: bool,
    narrowed: bool,
    extra_fds: bool,
    image_named: bool,
    provided: bool,
    launcher_named: bool,
    pager: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Helper,
    Launch,
}

/// Which spawn this is, or why it is none: what a launch alone gives asked
/// of a helper, or what a helper alone carries asked of a launch.
fn kind(shape: Shape) -> Result<Kind, &'static str> {
    if !shape.independent {
        return match (shape.detached, shape.provided, shape.launcher_named) {
            (true, _, _) => Err("only an independent launch is detached"),
            (_, true, _) => Err("only an independent launch carries a provided connector"),
            (_, _, true) => Err("only an independent launch goes through a launcher"),
            _ => Ok(Kind::Helper),
        };
    }
    if shape.narrowed {
        return Err("a launch runs its row, which no inheritance narrows");
    }
    match (shape.endowed, shape.extra_fds, shape.image_named) {
        (true, _, _) => Err("a launch carries no endowment"),
        (_, true, _) => Err("a launch carries descriptors 0 to 2 alone"),
        _ if shape.pager => Err("a launch starts its row as its row declares, never as a pager the caller names"),
        (_, _, true) => Err("a launch runs its row's own program, so it takes no image the caller names"),
        _ => Ok(Kind::Launch),
    }
}

impl<'a> Spawn<'a> {
    /// Which spawn this is, refused before anything moves where it asks what
    /// its kind does not give or would put a stream in another process.
    pub fn start(self) -> Result<Start<'a>, Refused> {
        self.refuse_streams()?;
        let shape = Shape {
            independent: self.independent,
            detached: self.parent == Parent::Detached,
            endowed: !self.endow.is_empty(),
            narrowed: self.inherit != Inherit::All,
            extra_fds: self.fds.iter().any(|&(fd, _, _)| fd > 2),
            image_named: self.image.is_some() || self.image_held,
            provided: !self.provided.is_empty(),
            launcher_named: self.launcher.is_some(),
            pager: self.pager.is_some(),
        };
        match kind(shape).map_err(Refused::Invalid)? {
            Kind::Helper => Ok(Start::Helper(Helper(self))),
            Kind::Launch => Ok(Start::Launch(Independent(self))),
        }
    }

    /// Refuses, before anything moves, a descriptor naming a stream's own
    /// handle: a holder stays in its process, and what goes is an end of a new
    /// one, which the caller exports. An endowment naming one, or a duplicate
    /// of one, is the kernel's to refuse: every handle of a stream carries
    /// `HELD_CONNECTION`, which has no `TRANSFER`.
    fn refuse_streams(&self) -> Result<(), Refused> {
        match self.fds.iter().map(|&(_, handle, _)| handle).find(|h| crate::net::stream::is_stream(*h)) {
            Some(stream) => Err(Refused::Stream(stream.raw())),
            None => Ok(()),
        }
    }
}

impl Helper<'_> {
    /// Spawn the child here, running `image`, which this process lays out
    /// ([`crate::elf::program`]) and the kernel pages the child from, and
    /// starting with the spawn's arguments and environment
    /// ([`crate::startup`]). Refused [`Refused::Invalid`] without one,
    /// or for one that names a layout the child could not start from.
    pub fn spawn(self, image: Option<&Image>) -> Result<Process, Refused> {
        let s = self.0;
        let image = image.ok_or(Refused::Invalid("a spawn names the program it runs"))?;
        let path = path_label(image_and_name(s.program, s.image).0);
        let name = name(s.argv.split(|&b| b == 0).find(|arg| !arg.is_empty()).unwrap_or(s.program.as_bytes()));
        let env = crate::files::starting_in(s.env, s.cwd)
            .ok_or(Refused::Invalid("a working directory that is not absolute, is too long or holds a NUL"))?;
        // What the child is endowed and what it calls each, in the order the
        // kernel installs them. Duplicates rather than this process's own
        // handles: an endowment is a move, and a parent that gave its
        // services or its stdout away could not spawn a second child. Every
        // handle is held here until the spawn has moved it: closed by a
        // refusal, given up by a spawn.
        let mut table: Vec<(String, Moved)> = Vec::new();
        let named: Vec<&str> = s.endow.iter().map(|(label, _)| *label).collect();
        let services: Vec<(&str, BorrowedHandle<'static>)> = crate::endow::inheritable().collect();
        let inherited = inherited(services.iter().map(|(label, _)| *label), s.inherit, &named);
        for (label, handle) in s.endow {
            table.push((String::from(label), handle));
        }
        for (label, connector) in services.iter().filter(|(label, _)| inherited.contains(label)) {
            table.push((String::from(*label), connector.try_clone_to_owned().map_err(Refused::Syscall)?.into()));
        }
        for &(fd, handle, kind) in s.fds {
            let copy = handle.try_clone_to_owned().map_err(Refused::Syscall)?;
            table.push((fd_label(fd, handle.raw(), kind, s.fds), copy.into()));
        }
        if table.len() > MAX_SPAWN_ENDOWMENTS {
            return Err(Refused::Invalid("more endowments and descriptors than a spawn carries"));
        }
        let names: Vec<&str> = table.iter().map(|(label, _)| label.as_str()).collect();
        if names.iter().enumerate().any(|(i, label)| names[..i].contains(label)) {
            return Err(Refused::Invalid("two endowments under one name"));
        }
        if names.contains(&"") {
            return Err(Refused::Invalid("an endowment under no name"));
        }
        let entries: Vec<TransferEntry> = table.iter().map(|(_, moved)| moved.entry()).collect();
        // Held until the spawn answers: the child copies its pages from it.
        let block = crate::startup::Block::new(s.argv, &env, &names, s.job).map_err(|why| match why {
            crate::startup::Refused::TooLarge => Refused::Invalid("arguments, an environment and names past what a child starts with"),
            crate::startup::Refused::Memory(e) => Refused::Syscall(e),
        })?;
        let program = crate::elf::program::lay_out(image.bytes(), crate::elf::Machine::RUNNING, image.source())
            .map_err(Refused::Invalid)?
            .starting_with(&block);
        let maps = program.maps();
        let build_id = program.build_id();
        let none = u64::from(toyos_abi::HANDLE_INVALID.0);
        let args = SpawnArgs {
            path_ptr: path.as_ptr() as u64,
            path_len: path.len() as u64,
            name_ptr: name.as_ptr() as u64,
            name_len: name.len() as u64,
            endow_ptr: entries.as_ptr() as u64,
            endow_count: entries.len() as u64,
            maps_ptr: maps.as_ptr() as u64,
            maps_count: maps.len() as u64,
            entry: program.entry,
            image_base: program.image_base,
            build_id_ptr: build_id.as_ptr() as u64,
            build_id_len: build_id.len() as u64,
            place: match s.parent {
                Parent::Caller => none,
                Parent::Place(place) => u64::from(place.raw().0),
                Parent::Detached => unreachable!("a detached child is refused a helper's spawn"),
            },
            job: s.job.map_or(none, |job| u64::from(job.raw().0)),
            pager: s.pager.map_or(none, |h| u64::from(h.raw().0)),
        };
        // SAFETY: every pointer is to a buffer above that outlives the call,
        // and every handle the endowment names is one `table` owns.
        match unsafe { syscall::spawn(&args) } {
            Ok(handle) => {
                // The spawn moved them into the child.
                table.into_iter().for_each(|(_, moved)| { moved.handle.into_raw(); });
                // SAFETY: the kernel installed this handle for this call and no other.
                Ok(unsafe { Process::from_raw(handle) })
            }
            // An endowment moves only on a spawn that happened, so `table`
            // still owns them, and closes them as it drops.
            Err(e) => Err(Refused::Syscall(e)),
        }
    }
}

impl Independent<'_> {
    /// Ask this process's launcher to start the program. Refused
    /// [`Refused::Permission`], with nothing spawned in its place, where this
    /// process holds no launcher, the launcher will not open, or no row
    /// declares the program or the launcher's row may not start it.
    ///
    /// The stdio handles, the place and the job are **duplicated** before
    /// they go, or a handle to this process's own job made: a launch moves
    /// what it carries.
    pub fn launch(self) -> Result<Process, Refused> {
        let s = self.0;
        for (i, &(_, handle, kind)) in s.fds.iter().enumerate() {
            if kind == FdKind::File && s.fds[..i].iter().any(|(_, prior, kind)| *kind == FdKind::File && prior.raw() == handle.raw()) {
                return Err(Refused::Invalid("a launch needs a distinct exported connection for each file descriptor"));
            }
        }
        let held = match s.launcher {
            Some(named) => named,
            None => crate::endow::launcher().ok_or(Refused::Permission("this process holds no launcher"))?.as_handle(),
        };
        let conn = crate::port::connect(held).map_err(|_| Refused::Permission("the launcher this process holds will not open"))?;
        let place = match s.parent {
            Parent::Detached => None,
            Parent::Place(place) => Some(place),
            Parent::Caller => Some(crate::endow::this_process().as_handle()),
        };
        if s.provided.len() + usize::from(place.is_some()) + 1 > MAX_LAUNCH_EXTRAS {
            return Err(Refused::Invalid("more connectors provided than a launch carries beside its place and job"));
        }
        if s.fds.len() > MAX_LAUNCH_FDS {
            return Err(Refused::Invalid("a launch carries descriptors 0 to 2 alone"));
        }
        let job = match s.job {
            Some(job) => job.try_clone_to_owned().map_err(Refused::Syscall)?,
            None => crate::job::own().map_err(Refused::Syscall)?.0,
        };
        let parent = match place {
            None => launch::Parent::Detached,
            Some(place) => launch::Parent::Place(place.try_clone_to_owned().map_err(Refused::Syscall)?),
        };
        let fds = s
            .fds
            .iter()
            .map(|&(child, handle, kind)| handle.try_clone_to_owned().map(|copy| (child, copy, kind)))
            .collect::<Result<Vec<_>, _>>()
            .map_err(Refused::Syscall)?;
        let request = Launch {
            program: s.program,
            argv: s.argv,
            env: s.env,
            cwd: s.cwd,
            extras: s.provided,
            fds,
            parent,
            job,
        };
        match launch::launch(&conn, request) {
            Ok(Outcome::Started(process)) => Ok(process),
            Ok(Outcome::NotPermitted) => {
                Err(Refused::Permission("no row declares the program, or the launcher's row may not start it"))
            }
            Ok(Outcome::Refused) | Err(LaunchError::Sent(_)) => Err(Refused::NotStarted),
            Ok(Outcome::Gone) => Err(Refused::Syscall(SyscallError::Gone)),
            Err(LaunchError::NotSent(IpcError::TooLarge)) => Err(Refused::Invalid("the launch does not fit one frame")),
            Err(LaunchError::NotSent(IpcError::Syscall(e))) => Err(Refused::Syscall(e)),
            Err(LaunchError::NotSent(_)) => Err(Refused::NotStarted),
        }
    }
}

/// The name a child run as `program`, its `argv[0]`, goes by in the kernel's
/// records (`SpawnArgs::name_ptr`): its last component, cut at a character to
/// [`MAX_PROCESS_NAME`] bytes, with `_` for a character a name does not hold,
/// and `?` where that leaves nothing.
fn name(program: &[u8]) -> String {
    let text = String::from_utf8_lossy(program);
    let mut name = String::new();
    for c in text.rsplit('/').next().unwrap_or_default().chars() {
        let c = if c.is_control() || c.is_whitespace() { '_' } else { c };
        if name.len() + c.len_utf8() > MAX_PROCESS_NAME {
            break;
        }
        name.push(c);
    }
    if name.is_empty() {
        name.push('?');
    }
    name
}

/// The path a child run from `path` is spawned under (`SpawnArgs::path_ptr`),
/// a label in the kernel's records that nothing opens: each character a
/// spawn's path may not hold ([`syscall::spawn_path_holds`]) replaced with
/// U+FFFD, cut at a character to [`MAX_PATH`] bytes. A file's name may hold any
/// of them, and its program still runs.
fn path_label(path: &str) -> String {
    let mut label = String::new();
    for c in path.chars() {
        let c = if syscall::spawn_path_holds(c) { c } else { char::REPLACEMENT_CHARACTER };
        if label.len() + c.len_utf8() > MAX_PATH {
            break;
        }
        label.push(c);
    }
    label
}

/// What a helper runs, and the name it runs it under: the image named, else
/// the program; `argv[0]` is the program's either way.
pub fn image_and_name<'a>(program: &'a str, image: Option<&'a str>) -> (&'a str, &'a str) {
    (image.unwrap_or(program), program)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plain spawn is a helper, whatever it endows or runs, and is never
    /// asked of the launcher.
    #[test]
    fn a_plain_spawn_is_a_helper() {
        for shape in [
            Shape::default(),
            Shape { endowed: true, ..Shape::default() },
            Shape { narrowed: true, ..Shape::default() },
            Shape { extra_fds: true, ..Shape::default() },
            Shape { image_named: true, ..Shape::default() },
            Shape { pager: true, ..Shape::default() },
        ] {
            assert_eq!(kind(shape), Ok(Kind::Helper), "{shape:?}");
        }
    }

    /// What only a launch gives is refused a helper, and what only a helper
    /// carries is refused a launch: neither becomes the other.
    #[test]
    fn each_kind_refuses_what_the_other_alone_takes() {
        const LAUNCH: Shape =
            Shape { independent: true, detached: false, endowed: false, narrowed: false, extra_fds: false, image_named: false, provided: false, launcher_named: false, pager: false };
        assert_eq!(kind(LAUNCH), Ok(Kind::Launch));
        assert_eq!(kind(Shape { detached: true, provided: true, launcher_named: true, ..LAUNCH }), Ok(Kind::Launch));
        for shape in [
            Shape { detached: true, ..Shape::default() },
            Shape { provided: true, ..Shape::default() },
            Shape { launcher_named: true, ..Shape::default() },
        ] {
            assert!(kind(shape).is_err(), "{shape:?}");
        }
        for shape in [
            Shape { endowed: true, ..LAUNCH },
            Shape { narrowed: true, ..LAUNCH },
            Shape { extra_fds: true, ..LAUNCH },
            Shape { image_named: true, ..LAUNCH },
            Shape { pager: true, ..LAUNCH },
        ] {
            assert!(kind(shape).is_err(), "{shape:?}");
        }
    }

    /// A helper is given every service its parent holds, those its spawn
    /// names alone, or none; and one the spawn endows itself is the spawn's.
    #[test]
    fn a_helper_inherits_the_services_its_spawn_admits() {
        let held = ["svc:fs:/tmp", "svc:net", "provide:surface"];
        let given = |inherit, endowed: &[&str]| inherited(held.into_iter(), inherit, endowed);
        assert_eq!(given(Inherit::All, &[]), held);
        assert_eq!(given(Inherit::All, &["fd:0", "launcher"]), held);
        assert_eq!(given(Inherit::All, &["svc:net"]), ["svc:fs:/tmp", "provide:surface"]);
        assert_eq!(given(Inherit::Only(&["fs:/tmp", "surface", "absent"]), &[]), ["svc:fs:/tmp", "provide:surface"]);
        assert_eq!(given(Inherit::Only(&["net"]), &["svc:net"]), [""; 0]);
        assert_eq!(given(Inherit::Only(&[]), &[]), [""; 0]);
        // A name is a service's, never a label's: `svc:net` names no service.
        assert_eq!(given(Inherit::Only(&["svc:net"]), &[]), [""; 0]);
    }

    /// A descriptor's label is its number and kind, and each file sharing one
    /// exported connection names the first descriptor that does.
    #[test]
    fn a_descriptor_is_labelled_by_its_number_and_kind() {
        // SAFETY: the borrows are numbers this test compares and never uses.
        let handle = |raw| unsafe { BorrowedHandle::borrow_raw(RawHandle(raw)) };
        let fds = [(0, handle(7), FdKind::Handle), (1, handle(9), FdKind::File), (2, handle(9), FdKind::File), (5, handle(11), FdKind::File), (6, handle(7), FdKind::Exported)];
        let labels: Vec<String> = fds.iter().map(|&(fd, h, kind)| fd_label(fd, h.raw(), kind, &fds)).collect();
        assert_eq!(labels, ["fd:0", "fd:1:file:1", "fd:2:file:1", "fd:5:file:5", "fd:6:exported"]);
        for (label, &(fd, _, kind)) in labels.iter().zip(&fds) {
            assert_eq!(crate::fd::endowed_fd(label), Some((fd as i32, kind)), "{label}");
        }
    }

    /// Whatever a program is called, its name is one the kernel takes, and an
    /// applet's is the applet's.
    #[test]
    fn a_name_is_the_program_s_last_component_as_the_kernel_takes_one() {
        use toyos_abi::syscall::process_name;
        assert_eq!(name(b"/system/bin/cat"), "cat");
        assert_eq!(name(b"ls"), "ls");
        assert_eq!(name(b"/apps/My App/run\tme"), "run_me");
        assert_eq!(name(b"/system/bin/test_rs_spawn_child_ends_first"), "test_rs_spawn_child_ends_fi");
        assert_eq!(name("/x/".as_bytes()), "?");
        assert_eq!(name(b""), "?");
        let wide = "\u{e9}".repeat(20);
        assert_eq!(name(wide.as_bytes()), "\u{e9}".repeat(13));
        for program in [&b"/system/bin/cat"[..], b"", b"a b\n", b"\xff\xfe", wide.as_bytes(), &[b'n'; 100]] {
            assert!(process_name(name(program).as_bytes()).is_some(), "{program:?}");
        }
    }

    /// Whatever a program's path, the label it is spawned under is one the
    /// kernel takes, so a file whose name holds a control character still runs.
    #[test]
    fn a_path_label_is_the_path_as_the_kernel_takes_one() {
        use toyos_abi::syscall::spawn_path;
        assert_eq!(path_label("/system/bin/cat"), "/system/bin/cat");
        assert_eq!(path_label("/home/My Programs/h\u{e9}llo"), "/home/My Programs/h\u{e9}llo");
        assert_eq!(path_label("/tmp/a\nb\x1b[2J\u{85}"), "/tmp/a\u{fffd}b\u{fffd}[2J\u{fffd}");
        let long = "p".repeat(MAX_PATH + 1);
        assert_eq!(path_label(&long), long[..MAX_PATH]);
        // Each one-byte control character becomes three bytes.
        let grows = "\u{1}".repeat(MAX_PATH);
        assert_eq!(path_label(&grows), "\u{fffd}".repeat(MAX_PATH / 3));
        for path in ["", "/system/bin/cat", "a\nb", "a\tb\0", "\u{7f}", &long, &grows] {
            assert!(spawn_path(path_label(path).as_bytes()).is_some(), "{path:?}");
        }
    }

    /// A spawn that names its image runs that image under its program's name:
    /// `argv[0]` is the program, never the image's path.
    #[test]
    fn a_named_image_runs_under_its_program_s_name() {
        assert_eq!(image_and_name("/bin/cat", Some("/tmp/other")), ("/tmp/other", "/bin/cat"));
        assert_eq!(image_and_name("/bin/cat", None), ("/bin/cat", "/bin/cat"));
    }
}
