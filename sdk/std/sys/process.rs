use super::env::{CommandEnv, CommandEnvs, CommandResolvedEnvs};
use crate::collections::BTreeMap;
pub use crate::ffi::OsString as EnvKey;
use crate::ffi::{OsStr, OsString};
use crate::num::NonZero;
use crate::path::Path;
use crate::process::StdioPipes;
use crate::sys::fs::File;
use crate::sys::pipe::Pipe;
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
    extra_slots: Vec<[u32; 2]>,
    endowments: Vec<(String, u32)>,
    provided: Vec<(String, u32)>,
    parent: toyos::spawn::Parent,
    /// The program the child runs, when it is not the one `argv[0]` names.
    image_path: Option<OsString>,
    prepared: Option<Prepared>,
    /// The program [`Command::prepare`] read off a file server, and the path
    /// it read: kept across a later working directory, so a spawn starts the
    /// bytes the caller saw ([`Command::prepared_image`]).
    image: Option<(OsString, toyos::process::Image)>,
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
            extra_slots: Vec::new(),
            endowments: Vec::new(),
            provided: Vec::new(),
            parent: toyos::spawn::Parent::Caller,
            image_path: None,
            prepared: None,
            image: None,
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
            if crate::sys::fs::is_served(Path::new(&source)) {
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

    /// Add an extra handle mapping for the child process.
    /// The child will see slot `child_slot` holding the parent's `parent_handle`.
    pub fn inherit_handle(&mut self, child_slot: u32, parent_handle: u32) {
        self.extra_slots.push([child_slot, parent_handle]);
    }

    /// Give the child `handle` under the name `label`.
    ///
    /// The handle is **moved**: after a successful spawn this process no longer
    /// holds it. A caller that wants to keep one duplicates it first.
    pub fn endow(&mut self, label: &str, handle: u32) {
        self.endowments.push((label.to_owned(), handle));
    }

    /// Give the child `connector` under `name`, on top of its manifest row.
    ///
    /// Routes the spawn through the launcher, which is the only thing that can
    /// build a child's authority out of the child's own declaration.
    pub fn provide(&mut self, name: &str, connector: u32) {
        self.provided.push((name.to_owned(), connector));
    }

    /// Place the child under the process `place` names rather than under this
    /// one.
    pub fn under(&mut self, place: u32) {
        self.parent = toyos::spawn::Parent::Place(toyos_abi::RawHandle(place));
    }

    /// Ask the supervisor to be the child's parent, through the launcher or not at all.
    pub fn under_supervisor(&mut self) {
        self.parent = toyos::spawn::Parent::Supervisor;
    }

    fn resolve_program(&self) -> io::Result<OsString> {
        let prog = self.program.to_str().unwrap_or("");
        if prog.contains('/') {
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

        let mut slot_map: Vec<[u32; 2]> = Vec::new();
        let mut child_pipes: Vec<Pipe> = Vec::new();
        // The pipes a file server appends a served file from; held until the
        // child has its own copies.
        let mut streams: Vec<toyos::Pipe> = Vec::new();
        let mut stdin_pipe: Option<Pipe> = None;
        let mut stdout_pipe: Option<Pipe> = None;
        let mut stderr_pipe: Option<Pipe> = None;
        Self::setup_slot(&mut slot_map, &mut child_pipes, &mut streams, &mut stdin_pipe, stdin, 0, true)?;
        Self::setup_slot(&mut slot_map, &mut child_pipes, &mut streams, &mut stdout_pipe, stdout, 1, false)?;
        Self::setup_slot(&mut slot_map, &mut child_pipes, &mut streams, &mut stderr_pipe, stderr, 2, false)?;
        let stdio = slot_map.len();
        slot_map.extend_from_slice(&self.extra_slots);

        let capture = self.env.capture();
        let env_buf = env_blob(&capture);
        let endow: Vec<(&str, toyos_abi::RawHandle)> =
            self.endowments.iter().map(|(label, h)| (label.as_str(), toyos_abi::RawHandle(*h))).collect();
        let provided: Vec<(&str, toyos_abi::RawHandle)> =
            self.provided.iter().map(|(name, h)| (name.as_str(), toyos_abi::RawHandle(*h))).collect();
        let request = toyos::spawn::Spawn {
            program,
            image: image_path,
            argv: &argv_buf,
            env: &env_buf,
            cwd: &cwd,
            slots: &slot_map,
            stdio,
            endow: &endow,
            provided: &provided,
            parent: self.parent,
        };
        let pipes = |stdin, stdout, stderr| StdioPipes { stdin, stdout, stderr };
        let mut answer = crate::vec![0u8; toyos::ipc::MAX_FRAME_LEN as usize];
        let home = match request.launch(&mut answer).map_err(refused)? {
            toyos::spawn::Launched::Started(handle) => {
                return Ok((Process { handle }, pipes(stdin_pipe, stdout_pipe, stderr_pipe)));
            }
            toyos::spawn::Launched::Direct { home } => home,
        };
        // A caller that named `HOME`, or removed it, decided it.
        let named = self.env.iter().any(|(key, _)| key == OsStr::new("HOME"));
        let mut direct_env = Vec::new();
        toyos::spawn::direct_env(&env_buf, named, home, &mut direct_env);
        // A program on a file server is read here, into a memory object of
        // this process's own that the kernel pages the child from: the kernel
        // opens only what it serves itself.
        let source = Path::new(image_path.unwrap_or(program));
        let image = match prepared_image {
            Some(image) => Some(image),
            None if crate::sys::fs::is_served(source) => Some(crate::sys::fs::read_image(source)?),
            None => None,
        };
        let spawned = request.direct(&direct_env, image.as_ref());
        // The child keeps the object alive; this process's handle, and with it
        // its own mapping, goes.
        drop(image);
        drop(child_pipes);
        drop(streams);
        let handle = spawned.map_err(refused)?;
        Ok((Process { handle }, pipes(stdin_pipe, stdout_pipe, stderr_pipe)))
    }

    fn setup_slot(
        slot_map: &mut Vec<[u32; 2]>,
        child_pipes: &mut Vec<Pipe>,
        streams: &mut Vec<toyos::Pipe>,
        parent_pipe: &mut Option<Pipe>,
        stdio: &Stdio,
        child_slot: u32,
        is_input: bool,
    ) -> io::Result<()> {
        match stdio {
            Stdio::Inherit => slot_map.push([child_slot, child_slot]),
            Stdio::MakePipe | Stdio::MakeTtyPipe => {
                let (r, w) = crate::sys::pipe::pipe()?;
                if matches!(stdio, Stdio::MakeTtyPipe) {
                    toyos_abi::syscall::mark_tty(toyos_abi::RawHandle(r.raw_fd() as u32));
                    toyos_abi::syscall::mark_tty(toyos_abi::RawHandle(w.raw_fd() as u32));
                }
                if is_input {
                    slot_map.push([child_slot, r.raw_fd() as u32]);
                    child_pipes.push(r);
                    *parent_pipe = Some(w);
                } else {
                    slot_map.push([child_slot, w.raw_fd() as u32]);
                    child_pipes.push(w);
                    *parent_pipe = Some(r);
                }
            }
            Stdio::InheritFile(file) => {
                let (handle, stream) = file.as_child_stdio()?;
                slot_map.push([child_slot, handle.0]);
                streams.extend(stream);
            }
            Stdio::InheritPipe(pipe) => slot_map.push([child_slot, pipe.raw_fd() as u32]),
            Stdio::ParentStdout => slot_map.push([child_slot, 1]),
            Stdio::ParentStderr => slot_map.push([child_slot, 2]),
            Stdio::Null => {}
        }
        Ok(())
    }
}

/// A path the kernel and the launcher take, which is UTF-8.
fn utf8<'a>(path: &'a OsStr, what: &'static str) -> io::Result<&'a str> {
    path.to_str().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, what))
}

fn refused(refused: toyos::spawn::Refused) -> io::Error {
    use toyos::spawn::Refused;
    match refused {
        Refused::Permission(why) => io::Error::new(io::ErrorKind::PermissionDenied, why),
        Refused::Invalid(why) => io::Error::new(io::ErrorKind::InvalidInput, why),
        Refused::Stream(handle) => {
            io::Error::new(io::ErrorKind::InvalidInput, toyos::net::stream::Misuse::Moved(handle).to_string())
        }
        Refused::NotStarted => io::Error::from(io::ErrorKind::Other),
        Refused::Syscall(e) => crate::sys::to_io_error(e),
    }
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
