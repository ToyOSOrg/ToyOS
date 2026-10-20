//! Starting a child: asked of the supervisor's launcher, or spawned by this
//! process. std's `Command` and libc's `posix_spawn` both start one here, in
//! two steps, so neither carries a rule of its own: [`Spawn::launch`], which
//! decides where the child goes and asks the launcher when it is to be asked,
//! and, when that answers [`Launched::Direct`], [`Spawn::direct`] with the
//! environment [`direct_env`] makes.
//!
//! **The supervisor starts a child only by a launch**, and a launch runs its
//! row's own program with no authority but its row's and the connectors
//! provided beside it. So a spawn asked of the supervisor that carries an
//! endowment or an extra slot, or names its image, is refused, as is one that
//! names its image and provides a connector; one that decided what its child
//! holds or runs is spawned directly; and one whose launch was not made is
//! spawned directly only where it asked nothing a launch alone gives.

use toyos_abi::syscall::{self, EndowEntry, SpawnArgs, SyscallError, MAX_SPAWN_ENDOWMENTS, MAX_SPAWN_LABELS_LEN, SVC_LABEL};

use crate::launch::{self, Launch, LaunchError, Outcome, MAX_LAUNCH_EXTRAS, MAX_LAUNCH_SLOTS};
use crate::process::{Image, Process};
use crate::{AsHandle, OwnedHandle, RawHandle};

/// Whom a child is placed under, whose end takes it down.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Parent {
    /// This process.
    Caller,
    /// The process a handle carrying `WRITE` names.
    Place(RawHandle),
    /// The supervisor, asked through the launcher.
    Supervisor,
}

/// What a spawn asks for. Every handle in it is the caller's and stays the
/// caller's but the endowed and provided ones, which a spawn that starts a
/// child moves.
pub struct Spawn<'a> {
    /// The program, as the caller resolved it: what a launch names and what a
    /// direct spawn runs when no `image` is named.
    pub program: &'a str,
    /// The program a direct spawn runs in place of `program`.
    pub image: Option<&'a str>,
    /// NUL-separated, `argv[0]` first.
    pub argv: &'a [u8],
    /// `KEY=VALUE\0` repeated: what a launch carries, and what [`direct_env`]
    /// makes a direct spawn's of.
    pub env: &'a [u8],
    /// Absolute: the kernel starts a child only where its spawn says.
    pub cwd: &'a str,
    /// `[child slot, caller's handle]`, installed in order, so a later one for
    /// a slot wins.
    pub slots: &'a [[u32; 2]],
    /// How many of `slots`, first, are the child's stdin, stdout and stderr,
    /// which a launch carries; any after them only a direct spawn installs.
    pub stdio: usize,
    /// `(label, handle)` the child is endowed, each moved.
    pub endow: &'a [(&'a str, RawHandle)],
    /// `(name, connector)` put in a launched child's namespace beside its
    /// row's, each moved.
    pub provided: &'a [(&'a str, RawHandle)],
    pub parent: Parent,
}

/// Why nothing was started.
#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    /// Not permitted, for this reason.
    Permission(&'static str),
    /// The spawn names more than it can carry, for this reason.
    Invalid(&'static str),
    /// It would put a stream's handle in another process, which
    /// `crate::net::stream` answers for.
    Stream(RawHandle),
    /// The supervisor did not start a program it declares, or its answer was lost.
    NotStarted,
    /// The kernel refused.
    Syscall(SyscallError),
}

/// Where [`Spawn::launch`] left the child.
pub enum Launched<'h> {
    /// The supervisor started it.
    Started(Process),
    /// This process spawns it ([`Spawn::direct`]), with the `HOME` the
    /// supervisor answered for it if it answered one.
    Direct { home: Option<&'h str> },
}

impl Spawn<'_> {
    /// Where the child goes: refused, launched, or to be spawned directly.
    /// `answer` holds the `HOME` an undeclared program's launch answers, and
    /// is at least `ipc::MAX_FRAME_LEN` long to take any.
    pub fn launch<'h>(&self, answer: &'h mut [u8]) -> Result<Launched<'h>, Refused> {
        self.refuse_streams()?;
        let asked = Asked {
            supervisor: self.parent == Parent::Supervisor,
            endowed: !self.endow.is_empty(),
            extra_slots: self.slots.len() > self.stdio,
            image_named: self.image.is_some(),
            provided: !self.provided.is_empty(),
        };
        match route(asked) {
            Route::Refused(why) => Err(Refused::Permission(why)),
            Route::Direct => Ok(Launched::Direct { home: None }),
            Route::Launch => self.ask_launcher(answer),
        }
    }

    /// Spawn the child here, carrying `env` and running `image` when one is
    /// given, which the kernel pages the child from. It inherits this
    /// process's namespace under `svc` unless the spawn endows one.
    pub fn direct(&self, env: &[u8], image: Option<&Image>) -> Result<Process, Refused> {
        let path = image_and_name(self.program, self.image).0;
        let names_svc = self.endow.iter().any(|(label, _)| *label == SVC_LABEL);
        if self.endow.len() + usize::from(!names_svc) > MAX_SPAWN_ENDOWMENTS {
            return Err(Refused::Invalid("more endowments than a spawn carries"));
        }
        let mut entries = [EndowEntry { label_off: 0, label_len: 0, handle: RawHandle(0), _pad: 0 }; MAX_SPAWN_ENDOWMENTS];
        let mut labels = [0u8; MAX_SPAWN_LABELS_LEN];
        let (mut count, mut used) = (0, 0);
        // A duplicate rather than the handle itself: an endowment is a move,
        // and a parent that gave its namespace away could not spawn a second
        // child.
        let inherited = match names_svc {
            true => None,
            false => match crate::endow::namespace() {
                Some(ns) => Some(syscall::dup(ns.as_handle()).map_err(Refused::Syscall)?),
                None => None,
            },
        };
        let release = |e| {
            if let Some(handle) = inherited {
                syscall::close(handle);
            }
            e
        };
        for (label, handle) in self.endow.iter().copied().chain(inherited.map(|h| (SVC_LABEL, h))) {
            let Some(room) = labels.get_mut(used..used + label.len()) else {
                return Err(release(Refused::Invalid("endowment labels past what a spawn carries")));
            };
            room.copy_from_slice(label.as_bytes());
            entries[count] = EndowEntry { label_off: used as u32, label_len: label.len() as u32, handle, _pad: 0 };
            (count, used) = (count + 1, used + label.len());
        }
        let (image, image_len) = image.map_or((0, 0), Image::spawn_words);
        let args = SpawnArgs {
            path_ptr: path.as_ptr() as u64,
            path_len: path.len() as u64,
            argv_ptr: self.argv.as_ptr() as u64,
            argv_len: self.argv.len() as u64,
            slot_map_ptr: self.slots.as_ptr() as u64,
            slot_map_count: self.slots.len() as u64,
            env_ptr: env.as_ptr() as u64,
            env_len: env.len() as u64,
            endow_ptr: entries.as_ptr() as u64,
            endow_count: count as u64,
            labels_ptr: labels.as_ptr() as u64,
            labels_len: used as u64,
            cwd_ptr: self.cwd.as_ptr() as u64,
            cwd_len: self.cwd.len() as u64,
            image,
            image_len,
            place: match self.parent {
                Parent::Caller => u64::from(toyos_abi::HANDLE_INVALID.0),
                Parent::Place(place) => u64::from(place.0),
                Parent::Supervisor => unreachable!("a child asked of the supervisor is launched or refused"),
            },
        };
        // SAFETY: every pointer is to a buffer above that outlives the call.
        match unsafe { syscall::spawn(&args) } {
            // SAFETY: the kernel installed this handle for this call and no other.
            Ok(handle) => Ok(unsafe { Process::from_raw(handle) }),
            // An endowment moves only on a spawn that happened, so the
            // duplicate is still this process's to close.
            Err(e) => Err(release(Refused::Syscall(e))),
        }
    }

    /// Refuses, before anything moves, a spawn that would put a stream's
    /// handle in another process: a stream belongs to the process that
    /// connected or accepted it.
    fn refuse_streams(&self) -> Result<(), Refused> {
        let slots = self.slots.iter().map(|&[_, handle]| RawHandle(handle));
        let given = self.endow.iter().chain(self.provided).map(|&(_, handle)| handle);
        match slots.chain(given).find(|h| crate::net::stream::is_stream(*h)) {
            Some(stream) => Err(Refused::Stream(stream)),
            None => Ok(()),
        }
    }

    /// Ask the supervisor to start this program, or answer
    /// [`Launched::Direct`] for a caller that cannot or a program the manifest
    /// does not declare — but a child asked of the supervisor is launched or
    /// refused, never spawned here.
    ///
    /// The stdio handles and the place are **duplicated** before they go: a
    /// launch moves what it carries.
    fn ask_launcher<'h>(&self, answer: &'h mut [u8]) -> Result<Launched<'h>, Refused> {
        let for_supervisor = self.parent == Parent::Supervisor;
        let unmade = |why, home| match unmade(why, for_supervisor) {
            Unlaunched::Direct => Ok(Launched::Direct { home }),
            Unlaunched::Refused(why) => Err(Refused::Permission(why)),
            Unlaunched::Failed(e) => Err(Refused::Syscall(e)),
        };
        let Some(conn) = crate::endow::launcher().and_then(|held| held.open(launch::LAUNCHER).ok()) else {
            return unmade(Unmade::NoLauncher { provided: !self.provided.is_empty() }, None);
        };
        // A place is one of the batch's extras. Refused rather than spawned
        // directly: that child would hold this process's namespace, not its row.
        if self.provided.len() + usize::from(!for_supervisor) > MAX_LAUNCH_EXTRAS {
            return Err(Refused::Invalid("more connectors provided than a launch carries beside its place"));
        }
        let stdio = &self.slots[..self.stdio];
        if stdio.len() > MAX_LAUNCH_SLOTS {
            return Err(Refused::Invalid("more than three stdio slots"));
        }
        let place = match self.parent {
            Parent::Supervisor => None,
            Parent::Place(place) => Some(place),
            Parent::Caller => Some(crate::endow::this_process().as_handle()),
        };
        let parent = match place {
            None => launch::Parent::Supervisor,
            Some(place) => match syscall::dup(place) {
                Ok(copy) => launch::Parent::Place(copy),
                Err(e) => return unmade(Unmade::PlaceNotCopied(e), None),
            },
        };
        let mut slots = [(0u32, RawHandle(0)); MAX_LAUNCH_SLOTS];
        let release = |slots: &[(u32, RawHandle)]| {
            slots.iter().for_each(|(_, h)| syscall::close(*h));
            if let launch::Parent::Place(copy) = parent {
                syscall::close(copy);
            }
        };
        for (i, &[child, handle]) in stdio.iter().enumerate() {
            match syscall::dup(RawHandle(handle)) {
                Ok(copy) => slots[i] = (child, copy),
                Err(e) => {
                    release(&slots[..i]);
                    return Err(Refused::Syscall(e));
                }
            }
        }
        let slots = &slots[..stdio.len()];
        let mut extras = [("", RawHandle(0)); MAX_LAUNCH_EXTRAS];
        extras[..self.provided.len()].copy_from_slice(self.provided);
        let request = Launch {
            program: self.program,
            argv: self.argv,
            env: self.env,
            cwd: self.cwd,
            extras: &extras[..self.provided.len()],
            slots,
            parent,
        };
        // **The send moved them.** Every arm but `NotSent` is past the point
        // where the duplicates left this table, so closing them would be this
        // process naming a handle it does not hold — which the kernel answers
        // by ending it. The launcher releases what it took.
        match launch::launch(&conn, &request, answer) {
            // SAFETY: the supervisor moved this handle into this table and holds none.
            Ok(Outcome::Started(handle)) => Ok(Launched::Started(unsafe { Process::from_raw(handle) })),
            Ok(Outcome::NotDeclared { home }) => unmade(Unmade::NotDeclared, Some(home)),
            Ok(Outcome::Refused) | Err(LaunchError::Sent(_)) => Err(Refused::NotStarted),
            Ok(Outcome::Gone) => Err(Refused::Syscall(SyscallError::Gone)),
            Err(LaunchError::NotSent(_)) => {
                release(slots);
                unmade(Unmade::NotSent, None)
            }
        }
    }
}

/// A copy of what this process's `slot` holds, for a child to inherit there,
/// or `None` for a slot it was started without: a stdio slot a spawn left
/// empty is empty in the child too, as POSIX has a closed descriptor. A
/// stream's slot is refused as [`Spawn::launch`] refuses its handle.
pub fn inherited(slot: u16) -> Result<Option<OwnedHandle>, Refused> {
    if let Some(stream) = crate::net::stream::stream_at(slot) {
        return Err(Refused::Stream(stream));
    }
    match syscall::dup_slot(slot) {
        Ok(copy) => Ok(Some(OwnedHandle(copy))),
        Err(SyscallError::NotFound) => Ok(None),
        Err(e) => Err(Refused::Syscall(e)),
    }
}

/// The environment a direct spawn carries: `env`, except `HOME`.
///
/// **A direct child's `HOME` is one its caller named or one the supervisor
/// answered for it, never the one this process was started with.** The
/// supervisor decides every program's `HOME` from its row, and a service's is
/// its own `/state/<name>`: a child the supervisor never saw would otherwise
/// carry a location decided for its parent alone. `named` says the caller named
/// `env`'s `HOME`, or named that there is none.
pub fn direct_env(env: &[u8], named: bool, from_supervisor: Option<&str>, out: &mut impl Extend<u8>) {
    let entries = env.split(|&b| b == 0).filter(|e| !e.is_empty());
    for entry in entries.filter(|e| named || !e.starts_with(b"HOME=")) {
        out.extend(entry.iter().copied().chain([0]));
    }
    if let (false, Some(home)) = (named, from_supervisor) {
        out.extend(b"HOME=".iter().chain(home.as_bytes()).copied().chain([0]));
    }
}

/// What a spawn asked for before it was routed.
#[derive(Clone, Copy, Default)]
struct Asked {
    supervisor: bool,
    endowed: bool,
    extra_slots: bool,
    image_named: bool,
    provided: bool,
}

/// Where a spawn goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    Direct,
    /// Asked of the launcher, where this process holds one.
    Launch,
    Refused(&'static str),
}

fn route(asked: Asked) -> Route {
    if asked.supervisor && (asked.endowed || asked.extra_slots) {
        return Route::Refused("the supervisor starts only a launch, which carries no endowment and no extra slot");
    }
    if asked.image_named && (asked.supervisor || asked.provided) {
        return Route::Refused("a launch runs its row's own program, so it takes no image the caller names");
    }
    match asked.endowed || asked.extra_slots || asked.image_named {
        true => Route::Direct,
        false => Route::Launch,
    }
}

/// Why a launch was not made.
enum Unmade {
    /// This process holds no launcher; `provided` is a spawn that provided a
    /// connector, which only a launch carries.
    NoLauncher { provided: bool },
    /// No row declares the program.
    NotDeclared,
    /// The launch never left this process.
    NotSent,
    /// The copy of its place a launch carries could not be made.
    PlaceNotCopied(SyscallError),
}

/// What a spawn whose launch was not made gets.
#[derive(Debug, PartialEq, Eq)]
enum Unlaunched {
    Direct,
    Refused(&'static str),
    /// Refused as the copy of its place was.
    Failed(SyscallError),
}

/// A launch not made is spawned directly only where the spawn asked nothing a
/// launch alone gives: not the supervisor, not a connector beside its row, and
/// not a place the launch was to carry, under which a direct child would hold
/// this process's namespace and not its row's.
fn unmade(why: Unmade, supervisor: bool) -> Unlaunched {
    match why {
        Unmade::PlaceNotCopied(e) => Unlaunched::Failed(e),
        Unmade::NoLauncher { provided: true } => {
            Unlaunched::Refused("only a launch carries a provided connector, and this process holds no launcher")
        }
        Unmade::NoLauncher { provided: false } | Unmade::NotDeclared | Unmade::NotSent => match supervisor {
            true => Unlaunched::Refused("the supervisor did not launch it"),
            false => Unlaunched::Direct,
        },
    }
}

/// What a direct spawn runs, and the name it runs it under: the image named,
/// else the program; `argv[0]` is the program's either way.
pub fn image_and_name<'a>(program: &'a str, image: Option<&'a str>) -> (&'a str, &'a str) {
    (image.unwrap_or(program), program)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUPERVISOR: Asked = Asked { supervisor: true, endowed: false, extra_slots: false, image_named: false, provided: false };

    /// What the supervisor would not start is refused here, and nothing is
    /// spawned in its place.
    #[test]
    fn the_supervisor_is_asked_only_by_a_launch_of_its_row_s_own_program() {
        for asked in [
            Asked { endowed: true, ..SUPERVISOR },
            Asked { extra_slots: true, ..SUPERVISOR },
            Asked { image_named: true, ..SUPERVISOR },
            Asked { image_named: true, provided: true, ..Asked::default() },
        ] {
            assert!(matches!(route(asked), Route::Refused(_)));
        }
        assert_eq!(route(SUPERVISOR), Route::Launch);
        for why in [Unmade::NoLauncher { provided: false }, Unmade::NotDeclared, Unmade::NotSent] {
            assert_eq!(unmade(why, true), Unlaunched::Refused("the supervisor did not launch it"));
        }
    }

    /// A spawn that decided what its child holds or runs is spawned directly,
    /// launcher or not; one that decided nothing is asked of the launcher, and
    /// spawned directly where it is not launched.
    #[test]
    fn a_spawn_that_decided_its_child_is_spawned_directly() {
        for asked in [
            Asked { endowed: true, ..Asked::default() },
            Asked { extra_slots: true, ..Asked::default() },
            Asked { image_named: true, ..Asked::default() },
        ] {
            assert_eq!(route(asked), Route::Direct);
        }
        assert_eq!(route(Asked::default()), Route::Launch);
        for why in [Unmade::NoLauncher { provided: false }, Unmade::NotDeclared, Unmade::NotSent] {
            assert_eq!(unmade(why, false), Unlaunched::Direct);
        }
    }

    /// A launch carries a copy of its place: one this process cannot copy
    /// refuses the launch as the copy was refused, and nothing is spawned
    /// directly in its place, whoever the spawn asked for.
    #[test]
    fn a_launch_whose_place_cannot_be_copied_is_refused_and_not_spawned_directly() {
        for supervisor in [false, true] {
            let e = SyscallError::PermissionDenied;
            assert_eq!(unmade(Unmade::PlaceNotCopied(e), supervisor), Unlaunched::Failed(e));
        }
    }

    /// Only a launch carries a provided connector: with no launcher to carry
    /// it, the spawn is refused rather than started without it.
    #[test]
    fn a_provided_connector_with_no_launcher_is_refused() {
        for supervisor in [false, true] {
            assert!(matches!(unmade(Unmade::NoLauncher { provided: true }, supervisor), Unlaunched::Refused(_)));
        }
    }

    /// A spawn that names its image runs that image under its program's name:
    /// `argv[0]` is the program, never the image's path.
    #[test]
    fn a_named_image_runs_under_its_program_s_name() {
        assert_eq!(image_and_name("/bin/cat", Some("/tmp/other")), ("/tmp/other", "/bin/cat"));
        assert_eq!(image_and_name("/bin/cat", None), ("/bin/cat", "/bin/cat"));
    }

    fn direct(env: &[u8], named: bool, home: Option<&str>) -> Vec<u8> {
        let mut out = Vec::new();
        direct_env(env, named, home, &mut out);
        out
    }

    /// The start-up `HOME` is never handed on: the supervisor's answer takes
    /// its place, or nothing does.
    #[test]
    fn a_direct_child_carries_the_supervisor_s_home_and_not_this_process_s() {
        let env = b"PATH=/bin\0HOME=/state/svc\0TERM=dumb\0";
        assert_eq!(direct(env, false, Some("/home/u")), b"PATH=/bin\0TERM=dumb\0HOME=/home/u\0");
        assert_eq!(direct(env, false, None), b"PATH=/bin\0TERM=dumb\0");
    }

    /// A `HOME` the caller named, or named away, is the child's, whatever the
    /// supervisor answered; a `HOMEDIR` is no `HOME`.
    #[test]
    fn a_home_the_caller_named_is_the_child_s() {
        let env = b"HOME=/x\0HOMEDIR=/y\0";
        assert_eq!(direct(env, true, Some("/home/u")), b"HOME=/x\0HOMEDIR=/y\0");
        assert_eq!(direct(b"HOMEDIR=/y\0", true, Some("/home/u")), b"HOMEDIR=/y\0");
        assert_eq!(direct(env, false, Some("/home/u")), b"HOMEDIR=/y\0HOME=/home/u\0");
    }
}
