//! Asking the session launcher to start a declared program: an independent
//! launch.
//!
//! **`SYS_SPAWN` can only give a child what the caller holds, and that is not
//! enough.** doom is started from a shell and doom is to hold sound; the shell
//! is not. Under inheritance alone doom could hold sound only if every shell
//! did. So the supervisor serves `launcher`: the caller names a `[programs]` key, the supervisor
//! looks up what the manifest says that program holds, builds it out of the
//! connectors and claims *the supervisor* holds, and spawns it.
//!
//! **The asker's authority is one connector**, and the policy is the manifest.
//! A launch adds nothing to what the caller could do itself: the stdio handles
//! and the extra connectors travel from the caller's own table, which is the
//! same bound `SYS_SPAWN` has. What it *adds* is that the child's namespace is
//! its own manifest row rather than a narrowing of its parent's.
//!
//! **A launch names the job its program joins**: the caller's own, or one the
//! caller made with the authority to start an independent job. The supervisor
//! spawns the program in it, so a launched program is its caller's helper unless
//! its caller holds that authority and says otherwise.
//!
//! **A launch names its parent**, by one of two words: a place, which is a
//! handle to a process carrying `WRITE` — the caller's own `self`, duplicated —
//! that the program is spawned under, so the caller's end takes it down; or
//! detached, under the launcher's owner, the one way for a program to outlive
//! whoever started it.
//!
//! The wire is a single frame plus one handle batch, and this module is both
//! halves of it — std's `Command` encodes and the supervisor decodes.

use alloc::string::String;
use alloc::vec::Vec;


use crate::fd::Kind;
use crate::ipc::{Connection, IpcError};
use crate::OwnedHandle;

/// The label a holder is endowed its launcher under, and the one name in that
/// namespace. **Never an entry of `svc`**, which std hands every direct spawn:
/// a launcher carries its holder's row and session as a badge, and a program
/// passes it on only by endowing it ([`crate::endow::launcher`]).
pub const LAUNCHER: &str = "launcher";

/// Ask the supervisor to start a program. Carries the request blob below; the stdio
/// handles and the extra connectors travel in the batch beside it.
pub const MSG_LAUNCH: u32 = 1;
/// It started. One `Process` handle travels with this.
pub const MSG_LAUNCHED: u32 = 2;
/// Not permitted: no row declares the program, its package does not answer
/// for it, or the caller's row may not start it. The reason is in the
/// launcher's log.
pub const MSG_NOT_PERMITTED: u32 = 3;
/// It is declared and did not start: no such file, a full table, a refused
/// endowment. The reason is in the supervisor's log, not in this frame — a caller can do
/// nothing differently about any of them.
pub const MSG_REFUSED: u32 = 4;
/// The place it named is a process whose end has begun: nothing started, and
/// nothing will under it.
pub const MSG_GONE: u32 = 5;

/// Handles beyond stdio one launch may carry from its caller: the connectors
/// it transfers, the place it names, and its job.
///
/// Policy on the primitive, refused by name. Five and not more because the
/// batch also carries the three stdio handles and
/// [`MAX_TRANSFER_HANDLES`](toyos_abi::syscall::MAX_TRANSFER_HANDLES) is eight:
/// a launch is one crossing, and splitting it into two would put a child's
/// authority in two batches that can arrive apart.
pub const MAX_LAUNCH_EXTRAS: usize = 5;

/// Descriptors a launch may give the child: stdin, stdout and stderr, and a
/// caller that wants anything else in a child's table spawns it directly.
pub const MAX_LAUNCH_FDS: usize = 3;

const HEADER: usize = 36;

/// One descriptor on the wire: its number, then its kind's word.
const FD_BYTES: usize = 8;

/// A descriptor's kind on the wire.
const fn kind_word(kind: Kind) -> u32 {
    match kind {
        Kind::Handle => 0,
        Kind::Exported => 1,
    }
}

/// The kind a word names, or `None`.
const fn kind_of(word: u32) -> Option<Kind> {
    match word {
        0 => Some(Kind::Handle),
        1 => Some(Kind::Exported),
        _ => None,
    }
}

/// The header's last word for a launch that names a place.
const PARENT_PLACE: u32 = 1;
/// The header's last word for a detached launch.
const PARENT_DETACHED: u32 = 2;

/// Whom a launch asks the supervisor to place the program under.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Parent<H> {
    /// The process a handle carrying `WRITE` names, moved as the batch's last
    /// handle.
    Place(H),
    /// The launcher's owner.
    Detached,
}

/// What a caller asks the supervisor to start.
///
/// `argv` and `env` are exactly the blobs `SYS_SPAWN` takes, so the launcher
/// path and the direct path build one thing rather than two.
///
/// **It owns every handle it names** (`H`, an [`OwnedHandle`] but where a
/// test encodes one), and [`launch`] consumes them: each is moved to the
/// launcher, or closed when the kernel refuses the move.
pub struct Launch<'a, H = OwnedHandle> {
    /// The program's path, exactly as the caller resolved it.
    ///
    /// **Not a `[programs]` key**, because `ls` is a symlink to
    /// `toybox` and the row that says what an applet may hold is
    /// `toybox`'s. The supervisor resolves the path to a key; the caller does not parse
    /// the manifest and there is deliberately no second reader of it.
    pub program: &'a str,
    /// NUL-separated, `argv[0]` first.
    pub argv: &'a [u8],
    /// `KEY=VALUE\0` repeated. **Carried rather than inherited**: without it a
    /// launched child would get the supervisor's environment, and `cd /tmp && ls` would
    /// list `/`.
    pub env: &'a [u8],
    pub cwd: &'a str,
    /// `(name, connector)` the caller transfers into the child's namespace, on
    /// top of the manifest's. This is how a terminal gives its shell the
    /// `surface` port it made — a name the supervisor cannot know, because there is one
    /// per terminal.
    pub extras: Vec<(&'a str, H)>,
    /// `(child descriptor, handle, kind)`, at most [`MAX_LAUNCH_FDS`] of them,
    /// which the launcher endows as `fd:<n>` or `fd:<n>:<kind>`. The handles
    /// are **moved**, so a caller passing on its own stdout duplicates first.
    pub fds: Vec<(u32, H, Kind)>,
    /// Whom the program is placed under. A place is **moved** with the batch,
    /// and counts against [`MAX_LAUNCH_EXTRAS`].
    pub parent: Parent<H>,
    /// The job the program joins, a handle carrying `WRITE`. **Moved** with the
    /// batch, its last handle, and counts against [`MAX_LAUNCH_EXTRAS`].
    pub job: H,
}

/// Why a request would not go on the wire.
#[derive(Debug, PartialEq, Eq)]
pub enum EncodeError {
    /// More than [`MAX_LAUNCH_EXTRAS`], or more than [`MAX_LAUNCH_FDS`].
    TooMany,
    /// The blobs do not fit one frame.
    TooLarge,
}

impl<H> Launch<'_, H> {
    /// Write the request blob into `buf`, answering its length.
    pub fn encode(&self, buf: &mut [u8]) -> Result<usize, EncodeError> {
        let (place, word) = match self.parent {
            Parent::Place(_) => (1, PARENT_PLACE),
            Parent::Detached => (0, PARENT_DETACHED),
        };
        if self.extras.len() + place + 1 > MAX_LAUNCH_EXTRAS || self.fds.len() > MAX_LAUNCH_FDS {
            return Err(EncodeError::TooMany);
        }
        let names_len: usize = self.extras.iter().map(|(n, _)| n.len() + 1).sum();
        let fds_len = self.fds.len() * FD_BYTES;
        let total = HEADER
            + fds_len
            + self.program.len()
            + self.argv.len()
            + self.env.len()
            + self.cwd.len()
            + names_len;
        if total > buf.len() || total > crate::ipc::MAX_FRAME_LEN as usize {
            return Err(EncodeError::TooLarge);
        }
        let lens = [
            self.fds.len() as u32,
            self.extras.len() as u32,
            fds_len as u32,
            self.program.len() as u32,
            self.argv.len() as u32,
            self.env.len() as u32,
            self.cwd.len() as u32,
            names_len as u32,
            word,
        ];
        for (i, v) in lens.iter().enumerate() {
            buf[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        let mut at = HEADER;
        let mut put = |bytes: &[u8], at: &mut usize| {
            buf[*at..*at + bytes.len()].copy_from_slice(bytes);
            *at += bytes.len();
        };
        for (fd, _, kind) in &self.fds {
            put(&fd.to_le_bytes(), &mut at);
            put(&kind_word(*kind).to_le_bytes(), &mut at);
        }
        put(self.program.as_bytes(), &mut at);
        put(self.argv, &mut at);
        put(self.env, &mut at);
        put(self.cwd.as_bytes(), &mut at);
        for (name, _) in &self.extras {
            put(name.as_bytes(), &mut at);
            put(&[0], &mut at);
        }
        Ok(total)
    }
}

/// A decoded request, borrowing the frame it arrived in.
///
/// **Every field is a peer's claim about itself**, so nothing here is trusted:
/// a length that does not fit refuses the whole frame, and the program name
/// resolves against the manifest or does not resolve at all.
pub struct Request<'a> {
    pub program: &'a str,
    pub argv: &'a [u8],
    pub env: &'a [u8],
    pub cwd: &'a str,
    pub extra_count: usize,
    fds: &'a [u8],
    names: &'a [u8],
    parent: u32,
}

impl<'a> Request<'a> {
    pub fn decode(buf: &'a [u8]) -> Option<Self> {
        if buf.len() < HEADER {
            return None;
        }
        let field = |i: usize| {
            u32::from_le_bytes([buf[i * 4], buf[i * 4 + 1], buf[i * 4 + 2], buf[i * 4 + 3]])
                as usize
        };
        let (fd_count, extra_count) = (field(0), field(1));
        let (fds_len, program_len, argv_len, env_len, cwd_len, names_len) =
            (field(2), field(3), field(4), field(5), field(6), field(7));
        let parent = field(8) as u32;
        if extra_count + usize::from(parent == PARENT_PLACE) + 1 > MAX_LAUNCH_EXTRAS
            || fd_count > MAX_LAUNCH_FDS
            || fds_len != fd_count * FD_BYTES
        {
            return None;
        }
        let total = HEADER
            .checked_add(fds_len)?
            .checked_add(program_len)?
            .checked_add(argv_len)?
            .checked_add(env_len)?
            .checked_add(cwd_len)?
            .checked_add(names_len)?;
        if total > buf.len() {
            return None;
        }
        let mut at = HEADER;
        let take = |len: usize, at: &mut usize| {
            let slice = &buf[*at..*at + len];
            *at += len;
            slice
        };
        let fds = take(fds_len, &mut at);
        let program = core::str::from_utf8(take(program_len, &mut at)).ok()?;
        let argv = take(argv_len, &mut at);
        let env = take(env_len, &mut at);
        let cwd = core::str::from_utf8(take(cwd_len, &mut at)).ok()?;
        let names = take(names_len, &mut at);
        if names.iter().filter(|&&b| b == 0).count() != extra_count {
            return None;
        }
        Some(Self { program, argv, env, cwd, extra_count, fds, names, parent })
    }

    /// Whom the request names as the parent: `None` for neither word, which
    /// is a launch the supervisor refuses.
    pub fn parent(&self) -> Option<Parent<()>> {
        match self.parent {
            PARENT_PLACE => Some(Parent::Place(())),
            PARENT_DETACHED => Some(Parent::Detached),
            _ => None,
        }
    }

    /// The handles the batch carries: the descriptors, the extras, the place,
    /// then the job.
    pub fn handle_count(&self) -> usize {
        self.fd_count() + self.extra_count + usize::from(self.parent == PARENT_PLACE) + 1
    }

    /// The child descriptors the first handles of the batch are for, in
    /// order, each with its kind: `None` for a word that names no kind.
    pub fn fds(&self) -> impl Iterator<Item = (u32, Option<Kind>)> + '_ {
        self.fds.as_chunks::<FD_BYTES>().0.iter().map(|c| {
            let [a, b, c, d, e, f, g, h] = *c;
            (u32::from_le_bytes([a, b, c, d]), kind_of(u32::from_le_bytes([e, f, g, h])))
        })
    }

    pub fn fd_count(&self) -> usize {
        self.fds.len() / FD_BYTES
    }

    /// The extra connectors' names, in the order their handles arrive.
    pub fn extra_names(&self) -> impl Iterator<Item = &'a str> {
        self.names.split(|&b| b == 0).filter(|s| !s.is_empty()).filter_map(|s| {
            core::str::from_utf8(s).ok()
        })
    }
}

/// Whether `path` is already the string every path syscall will act on.
///
/// The kernel normalizes before it opens anything (`kernel/src/vfs.rs`'s
/// `normalize` drops an empty component and `.`, and pops on `..`), so a path
/// meaning something else after that is one nothing can both classify and
/// spawn: `/apps/./x` is outside `/apps` to the supervisor's package
/// classification and inside it to every syscall.
pub fn is_canonical(path: &str) -> bool {
    path.starts_with('/')
        && !path.split('/').skip(1).any(|part| part.is_empty() || part == "." || part == "..")
}

/// What a launch names for a program its caller typed: a bare name left
/// alone for `PATH`, anything with a `/` made absolute against `cwd` and
/// normalized the way the kernel does. It lives beside [`is_canonical`]
/// because a resolver that drifted from the gate would refuse every relative
/// command.
pub fn program_path(program: &str, cwd: &str) -> String {
    match program.contains('/') {
        true => crate::files::absolute(cwd, program),
        false => String::from(program),
    }
}

/// Why a launch did not answer. The request's handles are consumed either
/// way: moved to the launcher, or closed with a request that did not go.
pub enum LaunchError {
    /// Nothing reached the launcher: the request does not fit one frame, or
    /// the kernel refused its frame, as it refuses a handle without
    /// `TRANSFER`.
    NotSent(IpcError),
    /// The frame went, and no answer came back that this protocol reads.
    Sent(IpcError),
}

/// What a launch answered.
pub enum Outcome {
    /// The handle the caller now holds for the child.
    Started(OwnedHandle),
    /// Not permitted ([`MSG_NOT_PERMITTED`]).
    NotPermitted,
    /// Declared, and it did not start.
    Refused,
    /// The place it named has begun to end.
    Gone,
}

/// Send one launch and read its answer.
///
/// The handles travel in the launch frame itself, and the `Process` handle
/// comes back in the answer's.
pub fn launch(conn: &Connection, request: Launch<'_>) -> Result<Outcome, LaunchError> {
    let mut buf = [0u8; crate::ipc::MAX_FRAME_LEN as usize];
    let len = request
        .encode(&mut buf)
        .map_err(|_| LaunchError::NotSent(IpcError::TooLarge))?;
    let Launch { extras, fds, parent, job, .. } = request;
    let place = match parent {
        Parent::Place(place) => Some(place),
        Parent::Detached => None,
    };
    // Descriptors, then extras, then the place, then the job, as `Request` reads them.
    let handles = fds.into_iter().map(|(_, h, _)| h).chain(extras.into_iter().map(|(_, h)| h)).chain(place).chain([job]);
    conn.send_bytes_with(MSG_LAUNCH, &buf[..len], handles).map_err(LaunchError::NotSent)?;
    let mut rx = crate::ipc::FrameRx::<0>::new();
    let frame = rx.recv(&conn).map_err(LaunchError::Sent)?;
    match frame.msg_type {
        MSG_LAUNCHED => match rx.take_handles::<1>() {
            Some([process]) => Ok(Outcome::Started(process)),
            None => Err(LaunchError::Sent(IpcError::Malformed)),
        },
        MSG_NOT_PERMITTED => Ok(Outcome::NotPermitted),
        MSG_REFUSED => Ok(Outcome::Refused),
        MSG_GONE => Ok(Outcome::Gone),
        _ => Err(LaunchError::Sent(IpcError::Malformed)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Checked against the gate itself, not a shape written down twice.
    #[test]
    fn what_a_shell_resolves_is_what_the_launcher_accepts() {
        let cwd = "/home/toy/reltest";
        for (typed, want) in [
            ("./foo", "/home/toy/reltest/foo"),
            ("../bin/foo", "/home/toy/bin/foo"),
            ("./a/./b/../c", "/home/toy/reltest/a/c"),
            ("sub/foo", "/home/toy/reltest/sub/foo"),
            ("/system/bin/toybox", "/system/bin/toybox"),
            ("/system//bin/./toybox", "/system/bin/toybox"),
            ("/../system/bin/toybox", "/system/bin/toybox"),
        ] {
            let got = program_path(typed, cwd);
            assert_eq!(got, want, "{typed:?} from {cwd:?}");
            assert!(is_canonical(&got), "{typed:?} resolved to {got:?}, which the supervisor refuses");
        }
        // A bare name is `PATH`'s, and that rule is left where it was.
        for bare in ["foo", "echo", "toybox"] {
            assert_eq!(program_path(bare, cwd), bare);
        }
    }

    #[test]
    fn a_path_the_normalizer_would_change_is_not_canonical() {
        assert!(is_canonical("/apps/toy/echo"));
        assert!(is_canonical("/system/bin/toybox"));
        for bad in ["/apps/./toy/echo", "/apps//toy/echo", "apps/toy/echo", "/tmp/../apps/toy/echo", "/apps/toy/echo/", "./echo", "", "/"] {
            assert!(!is_canonical(bad), "{bad:?} passed as canonical");
        }
    }

    /// The header's last word names the parent: a place, detached, or — any other
    /// word — neither, which the supervisor refuses.
    #[test]
    fn a_request_names_its_parent_by_one_word() {
        let mut buf = [0u8; 256];
        let request = |parent| Launch {
            program: "/system/bin/cat",
            argv: b"",
            env: b"",
            cwd: "/",
            extras: Vec::new(),
            fds: Vec::new(),
            parent,
            job: (),
        };
        for (parent, named) in [(Parent::Place(()), Parent::Place(())), (Parent::Detached, Parent::Detached)] {
            let len = request(parent).encode(&mut buf).expect("encode a launch");
            assert_eq!(Request::decode(&buf[..len]).expect("decode it").parent(), Some(named));
        }
        let len = request(Parent::Detached).encode(&mut buf).expect("encode a launch");
        for word in [0, 3, u32::MAX] {
            buf[HEADER - 4..HEADER].copy_from_slice(&word.to_le_bytes());
            assert_eq!(Request::decode(&buf[..len]).expect("decode it").parent(), None, "word {word}");
        }
    }

    /// Each descriptor goes on the wire with its kind, and a word that names
    /// no kind decodes as none, which the supervisor refuses.
    #[test]
    fn a_descriptor_crosses_with_its_kind() {
        let mut buf = [0u8; 256];
        let request = Launch {
            program: "/system/bin/cat",
            argv: b"",
            env: b"",
            cwd: "/",
            extras: Vec::new(),
            fds: vec![(0, (), Kind::Handle), (1, (), Kind::Exported)],
            parent: Parent::Detached,
            job: (),
        };
        let len = request.encode(&mut buf).expect("encode a launch");
        let decoded = Request::decode(&buf[..len]).expect("decode it");
        assert_eq!(decoded.fds().collect::<Vec<_>>(), [(0, Some(Kind::Handle)), (1, Some(Kind::Exported))]);
        assert_eq!(decoded.handle_count(), 3, "two descriptors and the job");
        let second_kind = HEADER + FD_BYTES + 4;
        buf[second_kind..second_kind + 4].copy_from_slice(&7u32.to_le_bytes());
        let decoded = Request::decode(&buf[..len]).expect("decode it");
        assert_eq!(decoded.fds().nth(1), Some((1, None)), "a word that names no kind");
    }

}
