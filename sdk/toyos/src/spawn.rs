//! Starting a child: a helper this process spawns, or an independent launch
//! asked of the session launcher it holds. std's `Command` and libc's
//! `posix_spawn` both start one here, so neither carries a rule of its own:
//! [`Spawn::start`] says which the spawn is, or refuses it, and what it
//! answers is the one way to start that kind.
//!
//! **A helper inherits.** It holds this process's namespace under `svc`, its
//! environment as given, and exactly the descriptors and endowments its spawn
//! names; never this process's launcher, which only an endowment the caller
//! names hands on.
//!
//! **An independent launch runs its row.** The launcher starts the program a
//! manifest row declares, holding that row's authority and the connectors
//! provided beside it, so a launch carries no endowment, no descriptor past
//! stdio and no image of the caller's choosing. A launch this process cannot
//! make is refused, never spawned as a helper instead.
//!
//! **The job the child joins is no endowment of that kind**: a launch carries
//! it too. The one endowed under [`JOB_LABEL`], or else this process's own.

use toyos_abi::syscall::{
    self, EndowEntry, SpawnArgs, SyscallError, FD_PREFIX, JOB_LABEL, MAX_SPAWN_ENDOWMENTS, MAX_SPAWN_LABELS_LEN,
    SVC_LABEL,
};

use crate::fd::Kind as FdKind;
use crate::ipc::IpcError;
use crate::launch::{self, Launch, LaunchError, Outcome, MAX_LAUNCH_EXTRAS, MAX_LAUNCH_FDS};
use crate::process::{Image, Process};
use alloc::vec::Vec;

use crate::{AsHandle, BorrowedHandle, OwnedHandle, RawHandle};

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
    /// Absolute: the kernel starts a child only where its spawn says.
    pub cwd: &'a str,
    /// `(child descriptor, caller's handle, its kind)`, each a number once:
    /// the child starts with these and no other descriptors. The caller keeps
    /// its handles, which a spawn duplicates; a stream's or a socket's is an
    /// end the caller exported for this child (`net::Exporter`), which no
    /// other child is given. A launch carries 0 to 2 alone.
    pub fds: &'a [(u32, BorrowedHandle<'a>, FdKind)],
    /// `(label, handle)` the child is endowed, each moved: by a spawn that
    /// happens to the child, and closed by one refused. One under
    /// [`JOB_LABEL`] is the job the child joins, which a launch carries too;
    /// any other only a helper takes.
    pub endow: Vec<(&'a str, OwnedHandle)>,
    /// `(name, connector)` put in a launched child's namespace beside its
    /// row's, each moved as `endow`'s are.
    pub provided: Vec<(&'a str, OwnedHandle)>,
    pub parent: Parent<'a>,
    /// An independent launch through this process's launcher, where a helper
    /// is not.
    pub independent: bool,
    /// A launcher this process holds, a namespace holding [`launch::LAUNCHER`],
    /// which a launch goes through in place of the one it was endowed:
    /// borrowed, and refused a helper.
    pub launcher: Option<BorrowedHandle<'a>>,
    /// A `SysCap` carrying `Rights::PAGER`, borrowed, which starts the child
    /// as a pager (`SpawnArgs::pager`): a helper's alone.
    pub pager: Option<BorrowedHandle<'a>>,
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
    match (shape.endowed, shape.extra_fds, shape.image_named) {
        (true, _, _) => Err("a launch carries no endowment but its job"),
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
            endowed: self.endow.iter().any(|(label, _)| *label != JOB_LABEL),
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
    /// Spawn the child here, carrying the spawn's environment and running
    /// `image` when one is given, which the kernel pages the child from.
    pub fn spawn(self, image: Option<&Image>) -> Result<Process, Refused> {
        let s = self.0;
        let path = image_and_name(s.program, s.image).0;
        let names_svc = s.endow.iter().any(|(label, _)| *label == SVC_LABEL);
        if s.endow.len() + s.fds.len() + usize::from(!names_svc) > MAX_SPAWN_ENDOWMENTS {
            return Err(Refused::Invalid("more endowments and descriptors than a spawn carries"));
        }
        let mut entries = [EndowEntry { label_off: 0, label_len: 0, handle: RawHandle(0), _pad: 0 }; MAX_SPAWN_ENDOWMENTS];
        let mut labels = [0u8; MAX_SPAWN_LABELS_LEN];
        let (mut count, mut used) = (0, 0);
        // Duplicates rather than the handles themselves: an endowment is a
        // move, and a parent that gave its namespace or its stdout away could
        // not spawn a second child. Every handle the spawn moves is held here
        // until it has: closed by a refusal, given up by a spawn.
        let mut moved: Vec<OwnedHandle> = Vec::new();
        let svc = match crate::endow::namespace().filter(|_| !names_svc) {
            Some(ns) => Some(ns.as_handle().try_clone_to_owned().map_err(Refused::Syscall)?),
            None => None,
        };
        let mut number = [0u8; 10];
        let mut put = |label: &[&[u8]], handle: RawHandle, count: &mut usize, used: &mut usize| {
            let len: usize = label.iter().map(|part| part.len()).sum();
            let room = labels.get_mut(*used..*used + len).ok_or(Refused::Invalid("endowment labels past what a spawn carries"))?;
            let mut at = 0;
            for part in label {
                room[at..at + part.len()].copy_from_slice(part);
                at += part.len();
            }
            entries[*count] = EndowEntry { label_off: *used as u32, label_len: len as u32, handle, _pad: 0 };
            (*count, *used) = (*count + 1, *used + len);
            Ok(())
        };
        for (label, handle) in s.endow {
            put(&[label.as_bytes()], handle.as_handle().raw(), &mut count, &mut used)?;
            moved.push(handle);
        }
        if let Some(svc) = svc {
            put(&[SVC_LABEL.as_bytes()], svc.as_handle().raw(), &mut count, &mut used)?;
            moved.push(svc);
        }
        for &(fd, handle, kind) in s.fds {
            let copy = handle.try_clone_to_owned().map_err(Refused::Syscall)?;
            let digits = decimal(fd, &mut number);
            let label = [FD_PREFIX.as_bytes(), digits, kind.suffix().as_bytes()];
            put(&label, copy.as_handle().raw(), &mut count, &mut used)?;
            moved.push(copy);
        }
        let (image, image_len) = image.map_or((0, 0), Image::spawn_words);
        let args = SpawnArgs {
            path_ptr: path.as_ptr() as u64,
            path_len: path.len() as u64,
            argv_ptr: s.argv.as_ptr() as u64,
            argv_len: s.argv.len() as u64,
            env_ptr: s.env.as_ptr() as u64,
            env_len: s.env.len() as u64,
            endow_ptr: entries.as_ptr() as u64,
            endow_count: count as u64,
            labels_ptr: labels.as_ptr() as u64,
            labels_len: used as u64,
            cwd_ptr: s.cwd.as_ptr() as u64,
            cwd_len: s.cwd.len() as u64,
            image,
            image_len,
            place: match s.parent {
                Parent::Caller => u64::from(toyos_abi::HANDLE_INVALID.0),
                Parent::Place(place) => u64::from(place.raw().0),
                Parent::Detached => unreachable!("a detached child is refused a helper's spawn"),
            },
            pager: s.pager.map_or(u64::from(toyos_abi::HANDLE_INVALID.0), |h| u64::from(h.raw().0)),
        };
        // SAFETY: every pointer is to a buffer above that outlives the call,
        // and every handle the endowment names is one `moved` owns.
        match unsafe { syscall::spawn(&args) } {
            Ok(handle) => {
                // The spawn moved them into the child.
                moved.into_iter().for_each(|handle| { handle.into_raw(); });
                // SAFETY: the kernel installed this handle for this call and no other.
                Ok(unsafe { Process::from_raw(handle) })
            }
            // An endowment moves only on a spawn that happened, so `moved`
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
    /// The stdio handles and the place are **duplicated** before they go, and
    /// a handle to this process's own job made: a launch moves what it
    /// carries. A job the spawn endowed goes itself, moved as an endowment is.
    pub fn launch(self) -> Result<Process, Refused> {
        let mut s = self.0;
        let held = match s.launcher {
            Some(named) => named.raw(),
            None => crate::endow::launcher().ok_or(Refused::Permission("this process holds no launcher"))?.as_handle().raw(),
        };
        let conn = syscall::namespace_open(held, launch::LAUNCHER)
            .map(|h| crate::ipc::Connection(crate::OwnedHandle(h)))
            .map_err(|_| Refused::Permission("the launcher this process holds will not open"))?;
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
        let job = match s.endow.iter().position(|(label, _)| *label == JOB_LABEL) {
            Some(at) => s.endow.swap_remove(at).1,
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
            Ok(Outcome::Started(handle)) => Ok(Process(handle)),
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

/// `n` in decimal, in `buf`.
fn decimal(mut n: u32, buf: &mut [u8; 10]) -> &[u8] {
    let mut at = buf.len();
    loop {
        at -= 1;
        buf[at] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            return &buf[at..];
        }
    }
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
            Shape { independent: true, detached: false, endowed: false, extra_fds: false, image_named: false, provided: false, launcher_named: false, pager: false };
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
            Shape { extra_fds: true, ..LAUNCH },
            Shape { image_named: true, ..LAUNCH },
            Shape { pager: true, ..LAUNCH },
        ] {
            assert!(kind(shape).is_err(), "{shape:?}");
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
