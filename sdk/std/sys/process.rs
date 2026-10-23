use super::env::{CommandEnv, CommandEnvs, CommandResolvedEnvs};
use crate::collections::BTreeMap;
pub use crate::ffi::OsString as EnvKey;
use crate::ffi::{OsStr, OsString};
use crate::num::NonZero;
use crate::path::Path;
use crate::process::StdioPipes;
use crate::sys::fs::File;
use crate::sys::pipe::Pipe;
use crate::sys::refused;
use crate::{fmt, io};

////////////////////////////////////////////////////////////////////////////////
// Command
////////////////////////////////////////////////////////////////////////////////

pub struct Command {
    program: OsString,
    args: Vec<OsString>,
    env: CommandEnv,

    cwd: Option<OsString>,
    stdin: Option<Stdio>,
    stdout: Option<Stdio>,
    stderr: Option<Stdio>,
    extra_fds: Vec<(u32, u32, toyos::fd::Kind)>,
    endowments: Vec<(String, u32)>,
    provided: Vec<(String, u32)>,
    parent: toyos::spawn::Parent,
    /// An independent launch, where a plain spawn is a helper.
    independent: bool,
    /// The program the child runs, when it is not the one `argv[0]` names.
    image_path: Option<OsString>,
    prepared: Option<Prepared>,
    /// The program [`Command::prepare`] read off a file server, and the path
    /// it read: kept across a later working directory, so a spawn starts the
    /// bytes the caller saw ([`Command::prepared_image`]).
    image: Option<(OsString, toyos::process::Image)>,
    /// A boot image this process holds, which the child runs instead of any
    /// file ([`Command::boot_image`]).
    boot_image: Option<u32>,
}

/// The file reads a spawn needs, made ahead of it ([`Command::prepare`]).
struct Prepared {
    program: OsString,
    cwd: String,
}

/// `KEY=VALUE\0` for each variable: the blob `SYS_SPAWN` and a launch both take.
fn env_blob(env: &BTreeMap<EnvKey, OsString>) -> Vec<u8> {
    let mut blob = Vec::new();
    for (key, value) in env {
        blob.extend_from_slice(key.as_encoded_bytes());
        blob.push(b'=');
        blob.extend_from_slice(value.as_encoded_bytes());
        blob.push(0);
    }
    blob
}

#[derive(Debug)]
pub enum Stdio {
    Inherit,
    Null,
    MakePipe,
    MakeTtyPipe,
    ParentStdout,
    ParentStderr,
    InheritFile(File),
    InheritPipe(Pipe),
}

impl Command {
    pub fn new(program: &OsStr) -> Command {
        Command {
            program: program.to_owned(),
            args: vec![program.to_owned()],
            env: Default::default(),
            cwd: None,
            stdin: None,
            stdout: None,
            stderr: None,
            extra_fds: Vec::new(),
            endowments: Vec::new(),
            provided: Vec::new(),
            parent: toyos::spawn::Parent::Caller,
            independent: false,
            image_path: None,
            prepared: None,
            image: None,
            boot_image: None,
        }
    }

    pub fn arg(&mut self, arg: &OsStr) {
        self.args.push(arg.to_owned());
    }

    /// Run the program at `path` rather than the one `argv[0]` names, which
    /// stays the child's first argument.
    pub fn image_from(&mut self, path: &OsStr) {
        self.image_path = Some(path.to_owned());
        self.prepared = None;
        self.image = None;
        self.boot_image = None;
    }

    /// Run the immutable region `handle` names, which this process keeps,
    /// rather than any file: `argv[0]` is the program the command names, and
    /// nothing opens it.
    pub fn boot_image(&mut self, handle: u32) {
        self.boot_image = Some(handle);
        self.image_path = None;
        self.prepared = None;
        self.image = None;
    }

    pub fn env_mut(&mut self) -> &mut CommandEnv {
        &mut self.env
    }

    pub fn cwd(&mut self, dir: &OsStr) {
        self.cwd = Some(dir.to_owned());
        self.prepared = None;
    }

    /// Find the program, judge the working directory and read a program on a
    /// file server, so that [`Self::spawn`] calls no file server for them. A
    /// program already read from the same path is kept, not read again.
    pub fn prepare(&mut self) -> io::Result<()> {
        let program = self.resolve_program()?;
        let cwd = self.child_cwd()?;
        let source = self.image_path.clone().unwrap_or_else(|| program.clone());
        let kept = self.image.as_ref().is_some_and(|(read, _)| *read == source);
        if !kept {
            self.image = None;
            if self.boot_image.is_none() && crate::sys::fs::is_served(Path::new(&source)) {
                let image = crate::sys::fs::read_image(Path::new(&source))?;
                self.image = Some((source, image));
            }
        }
        self.prepared = Some(Prepared { program, cwd });
        Ok(())
    }

    /// The program a prepared spawn starts, as [`Self::prepare`] read it off a
    /// file server: `None` before a prepare, and for a program the kernel
    /// opens itself.
    pub fn prepared_image(&self) -> Option<&[u8]> {
        self.prepared.as_ref()?;
        self.image.as_ref().map(|(_, image)| image.bytes())
    }

    pub fn stdin(&mut self, stdin: Stdio) {
        self.stdin = Some(stdin);
    }

    pub fn stdout(&mut self, stdout: Stdio) {
        self.stdout = Some(stdout);
    }

    pub fn stderr(&mut self, stderr: Stdio) {
        self.stderr = Some(stderr);
    }

    pub fn get_program(&self) -> &OsStr {
        &self.program
    }

    pub fn get_args(&self) -> CommandArgs<'_> {
        let mut iter = self.args.iter();
        iter.next();
        CommandArgs { iter }
    }

    pub fn get_envs(&self) -> CommandEnvs<'_> {
        self.env.iter()
    }

    pub fn get_env_clear(&self) -> bool {
        self.env.does_clear()
    }

    pub fn get_resolved_envs(&self) -> CommandResolvedEnvs {
        CommandResolvedEnvs::new(self.env.capture())
    }

    pub fn get_current_dir(&self) -> Option<&Path> {
        self.cwd.as_ref().map(|cs| Path::new(cs))
    }

    /// Give the child descriptor `child_fd`, naming this process's
    /// `parent_handle`, which this process keeps: the spawn duplicates it, or,
    /// for a stream's, gives the child a holder of the stream of its own.
    pub fn inherit_handle(&mut self, child_fd: u32, parent_handle: u32) {
        self.inherit(child_fd, parent_handle, toyos::fd::Kind::Handle);
    }

    /// Give the child descriptor `child_fd` as the stream end `end`, which
    /// `toyos::net::stream::export` made: the spawn duplicates it, and the
    /// child takes it up as a holder of the stream.
    pub fn inherit_exported(&mut self, child_fd: u32, end: u32) {
        self.inherit(child_fd, end, toyos::fd::Kind::Stream);
    }

    fn inherit(&mut self, child_fd: u32, handle: u32, kind: toyos::fd::Kind) {
        self.extra_fds.retain(|&(fd, _, _)| fd != child_fd);
        self.extra_fds.push((child_fd, handle, kind));
    }

    /// Give the child `handle` under the name `label`.
    ///
    /// The handle is **moved**: after a successful spawn this process no longer
    /// holds it. A caller that wants to keep one duplicates it first.
    pub fn endow(&mut self, label: &str, handle: u32) {
        self.endowments.push((label.to_owned(), handle));
    }

    /// Give the child `connector` under `name`, on top of its manifest row:
    /// an independent launch's alone.
    pub fn provide(&mut self, name: &str, connector: u32) {
        self.provided.push((name.to_owned(), connector));
    }

    /// Place the child under the process `place` names rather than under this
    /// one.
    pub fn under(&mut self, place: u32) {
        self.parent = toyos::spawn::Parent::Place(toyos_abi::RawHandle(place));
    }

    /// Start the child as an independent launch, through the launcher.
    pub fn independent(&mut self) {
        self.independent = true;
    }

    /// Place a launched child under the launcher's owner, so it outlives this process.
    pub fn detached(&mut self) {
        self.parent = toyos::spawn::Parent::Detached;
    }

    fn resolve_program(&self) -> io::Result<OsString> {
        let prog = self.program.to_str().unwrap_or("");
        if prog.contains('/') || self.boot_image.is_some() {
            return Ok(self.program.clone());
        }
        // Search PATH for the executable
        if let Some(path_var) = crate::env::var_os("PATH") {
            for dir in crate::env::split_paths(&path_var) {
                let candidate = dir.join(prog);
                if candidate.exists() {
                    return Ok(candidate.into_os_string());
                }
            }
        }
        Err(io::Error::from(io::ErrorKind::NotFound))
    }

    /// The directory the child starts in, always absolute: the kernel starts a
    /// child only where its spawn says, and a relative `cwd` is relative to ours.
    fn child_cwd(&self) -> io::Result<String> {
        let dir = match &self.cwd {
            Some(dir) if Path::new(dir).is_absolute() => Path::new(dir).to_path_buf(),
            Some(dir) => crate::env::current_dir()?.join(dir),
            None => crate::env::current_dir()?,
        };
        // The kernel judges a directory it serves itself; one on a file server
        // is judged here, by that server, since the kernel cannot.
        if crate::sys::fs::is_served(&dir) && !crate::fs::metadata(&dir)?.is_dir() {
            return Err(io::const_error!(
                io::ErrorKind::NotFound,
                "working directory is not a directory",
            ));
        }
        dir.into_os_string().into_string().map_err(|_| {
            io::const_error!(io::ErrorKind::InvalidInput, "working directory is not UTF-8")
        })
    }

    pub fn spawn(
        &mut self,
        default: Stdio,
        _needs_stdin: bool,
    ) -> io::Result<(Process, StdioPipes)> {
        let prepared = self.prepared.take();
        let image = self.image.take().map(|(_, image)| image);
        let (resolved, cwd, prepared_image) = match prepared {
            Some(Prepared { program, cwd }) => (program, cwd, image),
            None => (self.resolve_program()?, self.child_cwd()?, None),
        };
        let program = utf8(&resolved, "program path")?;
        let image_path = self.image_path.as_deref().map(|p| utf8(p, "image path")).transpose()?;
        // `argv[0]` is the program, never a named image's path.
        let mut argv_buf = Vec::new();
        argv_buf.extend_from_slice(resolved.as_encoded_bytes());
        for arg in &self.args[1..] {
            argv_buf.push(0);
            argv_buf.extend_from_slice(arg.as_encoded_bytes());
        }

        let stdin = self.stdin.as_ref().unwrap_or(&default);
        let stdout = self.stdout.as_ref().unwrap_or(&default);
        let stderr = self.stderr.as_ref().unwrap_or(&default);

        // The handles the child's descriptors are, each held here until the
        // spawn has duplicated it into the child.
        let mut held: Vec<(u32, toyos::OwnedHandle, toyos::fd::Kind)> = Vec::new();
        let mut stdin_pipe: Option<Pipe> = None;
        let mut stdout_pipe: Option<Pipe> = None;
        let mut stderr_pipe: Option<Pipe> = None;
        for (pipe, stdio, fd, is_input) in
            [(&mut stdin_pipe, stdin, 0, true), (&mut stdout_pipe, stdout, 1, false), (&mut stderr_pipe, stderr, 2, false)]
        {
            // A descriptor `inherit_handle` names is that handle's.
            if self.extra_fds.iter().any(|&(child, _, _)| child == fd) {
                continue;
            }
            if let Some((handle, kind)) = Self::child_stdio(pipe, stdio, fd, is_input)? {
                held.push((fd, handle, kind));
            }
        }
        let mut fds: Vec<(u32, toyos_abi::RawHandle, toyos::fd::Kind)> = Vec::new();
        for &(fd, handle, kind) in &self.extra_fds {
            let handle = toyos_abi::RawHandle(handle);
            if toyos::net::stream::is_stream(handle) {
                let end = toyos::net::stream::export(handle).map_err(|e| refused(toyos::spawn::Refused::Export(e)))?;
                held.push((fd, end, toyos::fd::Kind::Stream));
            } else {
                fds.push((fd, handle, kind));
            }
        }
        fds.extend(held.iter().map(|(fd, h, kind)| (*fd, toyos::AsHandle::as_handle(h), *kind)));

        let capture = self.env.capture();
        let env_buf = env_blob(&capture);
        let endow: Vec<(&str, toyos_abi::RawHandle)> =
            self.endowments.iter().map(|(label, h)| (label.as_str(), toyos_abi::RawHandle(*h))).collect();
        let provided: Vec<(&str, toyos_abi::RawHandle)> =
            self.provided.iter().map(|(name, h)| (name.as_str(), toyos_abi::RawHandle(*h))).collect();
        // A launch names its program absolute, as the launcher resolves it.
        let launched = self.independent.then(|| toyos::launch::program_path(program, &cwd));
        let request = toyos::spawn::Spawn {
            program: launched.as_deref().unwrap_or(program),
            image: image_path,
            image_held: prepared_image.is_some() || self.boot_image.is_some(),
            argv: &argv_buf,
            env: &env_buf,
            cwd: &cwd,
            fds: &fds,
            endow: &endow,
            provided: &provided,
            parent: self.parent,
            independent: self.independent,
        };
        let pipes = |stdin, stdout, stderr| StdioPipes { stdin, stdout, stderr };
        let helper = match request.start().map_err(refused)? {
            toyos::spawn::Start::Launch(launch) => {
                let handle = launch.launch().map_err(refused)?;
                return Ok((Process { handle }, pipes(stdin_pipe, stdout_pipe, stderr_pipe)));
            }
            toyos::spawn::Start::Helper(helper) => helper,
        };
        // A program on a file server is read here, into a memory object of
        // this process's own that the kernel pages the child from: the kernel
        // opens only what it serves itself.
        let source = Path::new(image_path.unwrap_or(program));
        let image = match (prepared_image, self.boot_image) {
            (Some(image), _) => Some(image),
            (None, Some(handle)) => {
                let copy = toyos_abi::syscall::dup(toyos_abi::RawHandle(handle)).map_err(crate::sys::to_io_error)?;
                // SAFETY: `dup` answered a fresh handle nothing else answers for.
                let copy = unsafe { toyos::OwnedHandle::from_raw(copy) };
                let region = toyos::shm::Immutable::adopt(copy).map_err(|why| {
                    io::Error::new(io::ErrorKind::InvalidInput, format!("the boot image is no immutable region: {why}"))
                })?;
                Some(toyos::process::Image::boot(region))
            }
            (None, None) if crate::sys::fs::is_served(source) => Some(crate::sys::fs::read_image(source)?),
            (None, None) => None,
        };
        let spawned = helper.spawn(image.as_ref());
        // The child keeps the object alive; this process's handle, and with it
        // its own mapping, goes.
        drop(image);
        drop(held);
        let handle = spawned.map_err(refused)?;
        Ok((Process { handle }, pipes(stdin_pipe, stdout_pipe, stderr_pipe)))
    }

    /// The handle the child's descriptor `fd` is, as `stdio` configures it,
    /// owned until the spawn has its own: `None` for one the child starts
    /// without. A pipe's other end is put in `parent_pipe`.
    fn child_stdio(
        parent_pipe: &mut Option<Pipe>,
        stdio: &Stdio,
        fd: u32,
        is_input: bool,
    ) -> io::Result<Option<(toyos::OwnedHandle, toyos::fd::Kind)>> {
        // What this process's own descriptor names, if it is open: one it
        // closed is closed in the child too.
        let inherit = |from: i32| toyos::fd::copy(from).map_err(refused);
        // SAFETY: each `dup` answers a fresh handle nothing else answers for.
        let copy = |pipe: &Pipe| pipe.with_handle(toyos_abi::syscall::dup)?.map(|h| unsafe { toyos::OwnedHandle::from_raw(h) }).map_err(crate::sys::to_io_error);
        let handle = |handle| Some((handle, toyos::fd::Kind::Handle));
        Ok(match stdio {
            Stdio::Inherit => inherit(fd as i32)?,
            Stdio::MakePipe | Stdio::MakeTtyPipe => {
                let (r, w) = crate::sys::pipe::pipe()?;
                if matches!(stdio, Stdio::MakeTtyPipe) {
                    r.with_handle(toyos_abi::syscall::mark_tty)?;
                    w.with_handle(toyos_abi::syscall::mark_tty)?;
                }
                let (theirs, ours) = if is_input { (r, w) } else { (w, r) };
                *parent_pipe = Some(ours);
                handle(copy(&theirs)?)
            }
            Stdio::InheritFile(file) => handle(file.as_child_stdio()?),
            Stdio::InheritPipe(pipe) => handle(copy(pipe)?),
            Stdio::ParentStdout => inherit(1)?,
            Stdio::ParentStderr => inherit(2)?,
            Stdio::Null => {
                use toyos_abi::handle::Rights;
                let direction = if is_input { Rights::READ } else { Rights::WRITE };
                let null = toyos::null(direction.union(Rights::DUP).union(Rights::TRANSFER).union(Rights::WAIT))
                    .map_err(crate::sys::to_io_error)?;
                handle(null)
            }
        })
    }
}

/// A path the kernel and the launcher take, which is UTF-8.
fn utf8<'a>(path: &'a OsStr, what: &'static str) -> io::Result<&'a str> {
    path.to_str().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, what))
}

impl From<ChildPipe> for Stdio {
    fn from(pipe: ChildPipe) -> Stdio {
        Stdio::InheritPipe(pipe)
    }
}

impl From<io::Stdout> for Stdio {
    fn from(_: io::Stdout) -> Stdio {
        Stdio::ParentStdout
    }
}

impl From<io::Stderr> for Stdio {
    fn from(_: io::Stderr) -> Stdio {
        Stdio::ParentStderr
    }
}

impl From<File> for Stdio {
    fn from(file: File) -> Stdio {
        Stdio::InheritFile(file)
    }
}

impl fmt::Debug for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if f.alternate() {
            let mut debug_command = f.debug_struct("Command");
            debug_command.field("program", &self.program).field("args", &self.args);
            if !self.env.is_unchanged() {
                debug_command.field("env", &self.env);
            }

            if self.cwd.is_some() {
                debug_command.field("cwd", &self.cwd);
            }

            if self.stdin.is_some() {
                debug_command.field("stdin", &self.stdin);
            }
            if self.stdout.is_some() {
                debug_command.field("stdout", &self.stdout);
            }
            if self.stderr.is_some() {
                debug_command.field("stderr", &self.stderr);
            }

            debug_command.finish()
        } else {
            if let Some(ref cwd) = self.cwd {
                write!(f, "cd {cwd:?} && ")?;
            }
            if self.env.does_clear() {
                write!(f, "env -i ")?;
            } else {
                let mut any_removed = false;
                for (key, value_opt) in self.get_envs() {
                    if value_opt.is_none() {
                        if !any_removed {
                            write!(f, "env ")?;
                            any_removed = true;
                        }
                        write!(f, "-u {} ", key.to_string_lossy())?;
                    }
                }
            }
            for (key, value_opt) in self.get_envs() {
                if let Some(value) = value_opt {
                    write!(f, "{}={value:?} ", key.to_string_lossy())?;
                }
            }
            if self.program != self.args[0] {
                write!(f, "[{:?}] ", self.program)?;
            }
            write!(f, "{:?}", self.args[0])?;

            for arg in &self.args[1..] {
                write!(f, " {:?}", arg)?;
            }
            Ok(())
        }
    }
}

/// How a child ended: a kill or a fault has no code and is never a success.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub struct ExitStatus(toyos::process::End);

impl Default for ExitStatus {
    fn default() -> Self {
        ExitStatus(toyos::process::End::Exited(0))
    }
}

impl ExitStatus {
    pub fn exit_ok(&self) -> Result<(), ExitStatusError> {
        if self.0.success() { Ok(()) } else { Err(ExitStatusError(self.0)) }
    }

    pub fn code(&self) -> Option<i32> {
        self.0.code()
    }

    /// See `os::toyos::process::ExitStatusExt::into_raw`.
    pub fn into_raw(self) -> u64 {
        self.0.to_word()
    }

    /// See `os::toyos::process::ExitStatusExt::signal`.
    pub fn signal(&self) -> Option<i32> {
        toyos::process::signal(self.0)
    }
}

impl fmt::Display for ExitStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use toyos::process::End;
        match self.0 {
            End::Exited(code) => write!(f, "exit status: {code}"),
            End::Killed => f.write_str("killed"),
            End::Fault(fault) => write!(f, "ended by a {} fault", fault.name()),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ExitStatusError(toyos::process::End);

impl Into<ExitStatus> for ExitStatusError {
    fn into(self) -> ExitStatus {
        ExitStatus(self.0)
    }
}

impl ExitStatusError {
    pub fn code(self) -> Option<NonZero<i32>> {
        self.0.code().and_then(NonZero::new)
    }
}

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub struct ExitCode(u8);

impl ExitCode {
    pub const SUCCESS: ExitCode = ExitCode(0);
    pub const FAILURE: ExitCode = ExitCode(1);

    pub fn as_i32(&self) -> i32 {
        self.0 as i32
    }
}

impl From<u8> for ExitCode {
    fn from(code: u8) -> Self {
        Self(code)
    }
}

/// A child, as the handle its spawn answered with.
///
/// There is no pid in here and no way back from one: what may wait for this
/// child, kill it or read its accounting is exactly what holds this handle.
pub struct Process {
    handle: toyos::process::Process,
}

impl Process {
    /// The child's pid, which is a name and not a key.
    ///
    /// Read out of the accounting record rather than kept beside the handle:
    /// this is a diagnostic and nothing in the tree calls it on a hot path, so
    /// paying a syscall for it is better than a second copy of the identity.
    /// Zero for a child whose accounting the kernel would not answer for, which
    /// is a process torn down between the spawn and the question.
    pub fn id(&self) -> u32 {
        self.handle.stats().map_or(0, |s| s.pid)
    }

    pub fn kill(&mut self) -> io::Result<()> {
        self.handle.kill().map_err(|_| io::Error::from(io::ErrorKind::Other))
    }

    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        self.handle.wait().map(ExitStatus).map_err(|_| io::Error::from(io::ErrorKind::Other))
    }

    /// See `os::toyos::process::ChildExt::as_raw_handle`.
    pub fn as_raw_handle(&self) -> u32 {
        toyos::AsHandle::as_handle(&self.handle).0
    }

    /// Give up the handle. See `os::toyos::process::ChildExt::into_raw_handle`.
    pub fn into_raw_handle(self) -> u32 {
        self.handle.into_raw().0
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        match self.handle.try_wait() {
            Ok(end) => Ok(Some(ExitStatus(end))),
            Err(toyos_abi::syscall::SyscallError::WouldBlock) => Ok(None),
            Err(_) => Err(io::Error::from(io::ErrorKind::Other)),
        }
    }
}

pub struct CommandArgs<'a> {
    iter: crate::slice::Iter<'a, OsString>,
}

impl<'a> Iterator for CommandArgs<'a> {
    type Item = &'a OsStr;
    fn next(&mut self) -> Option<&'a OsStr> {
        self.iter.next().map(|os| &**os)
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.iter.size_hint()
    }
}

impl<'a> ExactSizeIterator for CommandArgs<'a> {
    fn len(&self) -> usize {
        self.iter.len()
    }
    fn is_empty(&self) -> bool {
        self.iter.is_empty()
    }
}

impl<'a> fmt::Debug for CommandArgs<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.iter.clone()).finish()
    }
}

pub type ChildPipe = Pipe;

pub fn getpid() -> u32 {
    toyos_abi::syscall::getpid().0
}

pub fn read_output(
    out: ChildPipe,
    stdout: &mut Vec<u8>,
    err: ChildPipe,
    stderr: &mut Vec<u8>,
) -> io::Result<()> {
    // Read both pipes concurrently to avoid deadlock: if the child fills one
    // pipe buffer while we're blocking on the other, both sides stall.
    use crate::thread;
    let err_thread = thread::spawn(move || {
        let mut buf = Vec::new();
        err.read_to_end(&mut buf).map(|_| buf)
    });
    out.read_to_end(stdout)?;
    match err_thread.join() {
        Ok(Ok(buf)) => {
            *stderr = buf;
            Ok(())
        }
        Ok(Err(e)) => Err(e),
        Err(_) => Err(io::Error::new(io::ErrorKind::Other, "stderr reader thread panicked")),
    }
}
