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
    parent: Parent,
    /// The program the child runs, when it is not the one `argv[0]` names.
    image_path: Option<OsString>,
    prepared: Option<Prepared>,
}

/// Whom a child is placed under, whose end takes it down.
#[derive(Clone, Copy)]
enum Parent {
    /// This process.
    Caller,
    /// The process a handle carrying `WRITE` names.
    Place(u32),
    /// The supervisor, asked through the launcher.
    Supervisor,
}

/// The file reads a spawn needs, made ahead of it ([`Command::prepare`]).
struct Prepared {
    program: OsString,
    cwd: String,
    /// The program, when it is on a file server.
    image: Option<toyos::process::Image>,
}

/// Where a spawn goes once the launcher has been asked, or could not be.
enum Routed {
    /// The supervisor started it.
    Started(Process),
    /// This process spawns it, with the `HOME` the supervisor answered for it if it did.
    Direct { home: Option<OsString> },
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
            parent: Parent::Caller,
            image_path: None,
            prepared: None,
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
    }

    pub fn env_mut(&mut self) -> &mut CommandEnv {
        &mut self.env
    }

    pub fn cwd(&mut self, dir: &OsStr) {
        self.cwd = Some(dir.to_owned());
        self.prepared = None;
    }

    /// Find the program, judge the working directory and read a program on a
    /// file server, so that [`Self::spawn`] calls no file server for them.
    pub fn prepare(&mut self) -> io::Result<()> {
        let program = self.resolve_program()?;
        let cwd = self.child_cwd()?;
        let source = Path::new(self.image_path.as_deref().unwrap_or(&program));
        let image = match crate::sys::fs::is_served(source) {
            true => Some(crate::sys::fs::read_image(source)?),
            false => None,
        };
        self.prepared = Some(Prepared { program, cwd, image });
        Ok(())
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
        self.parent = Parent::Place(place);
    }

    /// Ask the supervisor to be the child's parent, through the launcher or not at all.
    pub fn under_supervisor(&mut self) {
        self.parent = Parent::Supervisor;
    }

    /// A duplicate of this process's own namespace handle, for the child to be
    /// endowed under `svc`.
    ///
    /// `None` when the caller endowed one itself — it has decided what its
    /// child may reach — and when this process has no namespace, which is a
    /// program the manifest gives no `receives` and whose children therefore
    /// have nothing to inherit.
    fn inherited_namespace(&self) -> Option<toyos_abi::RawHandle> {
        if self.endowments.iter().any(|(label, _)| label == toyos_abi::syscall::SVC_LABEL) {
            return None;
        }
        let namespace = toyos::endow::namespace()?;
        toyos_abi::syscall::dup(toyos::AsHandle::as_handle(namespace)).ok()
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
        // The supervisor is reached only by a launch, which carries no endowment and no
        // slot beyond stdio.
        if matches!(self.parent, Parent::Supervisor)
            && (!self.endowments.is_empty() || !self.extra_slots.is_empty())
        {
            return Err(io::const_error!(
                io::ErrorKind::PermissionDenied,
                "the supervisor starts only a launch, which carries no endowment and no extra slot",
            ));
        }
        // A launch runs the program its row names, never one the caller chose.
        if self.image_path.is_some()
            && (matches!(self.parent, Parent::Supervisor) || !self.provided.is_empty())
        {
            return Err(io::const_error!(
                io::ErrorKind::PermissionDenied,
                "a launch runs its row's own program, so it takes no image the caller names",
            ));
        }
        let prepared = self.prepared.take();
        let (resolved, cwd, prepared_image) = match prepared {
            Some(Prepared { program, cwd, image }) => (program, cwd, image),
            None => (self.resolve_program()?, self.child_cwd()?, None),
        };
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

        // Resolve each stdio to a slot_map entry: [child_slot, parent_handle]
        Self::setup_slot(&mut slot_map, &mut child_pipes, &mut streams, &mut stdin_pipe, stdin, 0, true)?;
        Self::setup_slot(&mut slot_map, &mut child_pipes, &mut streams, &mut stdout_pipe, stdout, 1, false)?;
        Self::setup_slot(&mut slot_map, &mut child_pipes, &mut streams, &mut stderr_pipe, stderr, 2, false)?;

        // Add extra handle mappings (e.g., for jobserver pipes)
        slot_map.extend_from_slice(&self.extra_slots);

        let capture = self.env.capture();
        let env_buf = env_blob(&capture);

        // **The routing rule.**
        // A caller that endowed a handle, named an extra slot or named the image
        // has decided what its child holds or runs, and the launcher would
        // overwrite that decision with a manifest row — so those spawn directly.
        // Everything else asks the launcher when it holds one, and falls back for
        // a program the image does not declare. A caller with no `launcher` connector gets plain
        // inheritance, which is what a program endowed nothing should get —
        // of everything but `HOME` (`direct_env`). A child asked of the supervisor is a
        // launch or nothing.
        let decided = !self.endowments.is_empty()
            || !self.extra_slots.is_empty()
            || self.image_path.is_some();
        let mut home_from_supervisor = None;
        if !decided {
            match self.launch(&resolved, &argv_buf, &env_buf, &cwd, &slot_map)? {
                Routed::Started(process) => {
                    drop(child_pipes);
                    return Ok((
                        process,
                        StdioPipes { stdin: stdin_pipe, stdout: stdout_pipe, stderr: stderr_pipe },
                    ));
                }
                Routed::Direct { home } => home_from_supervisor = home,
            }
        }
        // The label blob and the entries that index it, made after the routing:
        // a launch carries neither, and a namespace copy made for one would be a
        // handle nothing closes. Built here because the kernel reads both out of
        // one call and keeps the blob for the child's life.
        let mut labels = Vec::new();
        let mut endow = Vec::with_capacity(self.endowments.len() + 1);
        let mut push = |label: &str, handle: u32, labels: &mut Vec<u8>| {
            endow.push(toyos_abi::syscall::EndowEntry {
                label_off: labels.len() as u32,
                label_len: label.len() as u32,
                handle: toyos_abi::RawHandle(handle),
                _pad: 0,
            });
            labels.extend_from_slice(label.as_bytes());
        };
        for (label, handle) in &self.endowments {
            push(label, *handle, &mut labels);
        }
        // The child inherits this process's namespace unless the caller decided
        // otherwise. A duplicate rather than the handle itself: an endowment is
        // a move, and a parent that gave its namespace away could not spawn a
        // second child. A caller that endows `svc` has decided what its child
        // may reach and is not overruled here.
        let inherited = self.inherited_namespace();
        if let Some(handle) = inherited {
            push(toyos_abi::syscall::SVC_LABEL, handle.0, &mut labels);
        }
        let env_buf = env_blob(&self.direct_env(capture, home_from_supervisor));
        // A program on a file server is read here, into a memory object of
        // this process's own that the kernel pages the child from: the kernel
        // opens only what it serves itself.
        let source = self.image_path.as_deref().unwrap_or(&resolved);
        let image = match prepared_image {
            Some(image) => Some(image),
            None if crate::sys::fs::is_served(Path::new(source)) => {
                Some(crate::sys::fs::read_image(Path::new(source)).map_err(|e| {
                    if let Some(handle) = inherited {
                        toyos_abi::syscall::close(handle);
                    }
                    e
                })?)
            }
            None => None,
        };

        let spawn_args = toyos_abi::syscall::SpawnArgs {
            path_ptr: source.as_encoded_bytes().as_ptr().expose_provenance() as u64,
            path_len: source.len() as u64,
            argv_ptr: argv_buf.as_ptr().expose_provenance() as u64,
            argv_len: argv_buf.len() as u64,
            slot_map_ptr: slot_map.as_ptr().expose_provenance() as u64,
            slot_map_count: slot_map.len() as u64,
            env_ptr: env_buf.as_ptr().expose_provenance() as u64,
            env_len: env_buf.len() as u64,
            endow_ptr: endow.as_ptr().expose_provenance() as u64,
            endow_count: endow.len() as u64,
            labels_ptr: labels.as_ptr().expose_provenance() as u64,
            labels_len: labels.len() as u64,
            cwd_ptr: cwd.as_ptr().expose_provenance() as u64,
            cwd_len: cwd.len() as u64,
            image: image.as_ref().map_or(0, |image| image.spawn_words().0),
            image_len: image.as_ref().map_or(0, |image| image.spawn_words().1),
            place: match self.parent {
                Parent::Caller => u64::from(toyos_abi::HANDLE_INVALID.0),
                Parent::Place(place) => u64::from(place),
                Parent::Supervisor => {
                    unreachable!("a child asked of the supervisor is launched or refused")
                }
            },
        };
        // SAFETY: spawn_args contains valid pointers to stack-local buffers that outlive the call.
        let spawned = unsafe { toyos_abi::syscall::spawn(&spawn_args) };
        // The child keeps the object alive; this process's handle, and with it
        // its own mapping, goes.
        drop(image);

        // Close child-side pipe ends in the parent
        drop(child_pipes);
        drop(streams);

        let handle = spawned.map_err(|e| {
            // An endowment moves only on a spawn that happened, so the
            // duplicate this call made is still ours and is ours to close.
            if let Some(handle) = inherited {
                toyos_abi::syscall::close(handle);
            }
            crate::sys::to_io_error(e)
        })?;

        Ok((
            // SAFETY: the kernel installed this handle in our table for this
            // call and no other.
            Process { handle: unsafe { toyos::process::Process::from_raw(handle) } },
            StdioPipes { stdin: stdin_pipe, stdout: stdout_pipe, stderr: stderr_pipe },
        ))
    }

    /// The environment a direct spawn carries: the caller's, except `HOME`.
    ///
    /// **A direct child's `HOME` is one its caller named or one the supervisor answered
    /// for it, never the one this process was started with.** The supervisor decides
    /// every program's `HOME` from its row, and a service's is its own
    /// `/state/<name>`: a child the supervisor never saw would otherwise carry a location
    /// decided for its parent alone.
    fn direct_env(
        &self,
        mut env: BTreeMap<EnvKey, OsString>,
        from_supervisor: Option<OsString>,
    ) -> BTreeMap<EnvKey, OsString> {
        let home = OsStr::new("HOME");
        let named = self.env.iter().find(|(key, _)| *key == home).map(|(_, value)| value);
        env.remove(home);
        match named {
            Some(Some(value)) => {
                env.insert(home.to_owned(), value.to_owned());
            }
            Some(None) => {}
            None => {
                if let Some(value) = from_supervisor {
                    env.insert(home.to_owned(), value);
                }
            }
        }
        env
    }

    /// Ask the supervisor to start this program, or answer [`Routed::Direct`] for
    /// a caller that cannot or a program the manifest does not declare — but a
    /// child asked of the supervisor is launched or refused `PermissionDenied`, never
    /// spawned here.
    ///
    /// The stdio handles and the place are **duplicated** before they go: a
    /// launch moves what it carries, and `Stdio::Inherit` names the parent's
    /// own slot 1.
    fn launch(
        &self,
        resolved: &OsStr,
        argv: &[u8],
        env: &[u8],
        cwd: &str,
        slot_map: &[[u32; 2]],
    ) -> io::Result<Routed> {
        use toyos::launch::{self, Launch, LaunchError, MAX_LAUNCH_EXTRAS, Outcome};

        let for_supervisor = matches!(self.parent, Parent::Supervisor);
        // Where a launch that is not made goes: the direct spawn, which places
        // the child where this one would have, or nowhere for the supervisor.
        let direct = |home: Option<OsString>| {
            if for_supervisor {
                Err(io::const_error!(
                    io::ErrorKind::PermissionDenied,
                    "the supervisor did not launch it"
                ))
            } else {
                Ok(Routed::Direct { home })
            }
        };

        // A `provide` is a statement that the child's authority comes from its
        // own manifest row plus this connector, and only the launcher can build
        // that. Without one there is no weaker thing to fall back to that would
        // still be what the caller asked for.
        let Some(conn) =
            toyos::endow::launcher().and_then(|held| held.open(toyos::launch::LAUNCHER).ok())
        else {
            return if self.provided.is_empty() {
                direct(None)
            } else {
                Err(io::Error::from(io::ErrorKind::PermissionDenied))
            };
        };

        // The whole path, not a key: `/bin/ls` is a symlink to `/bin/toybox`
        // and the row that says what an applet holds is `toybox`'s. Resolving
        // that is the supervisor's — it holds the manifest and this process must not
        // become a second reader of it.
        let program = resolved.to_str().unwrap_or("");

        // A place is one of the batch's extras. Refused rather than spawned
        // directly: that child would hold this process's namespace, not its row.
        if self.provided.len() + usize::from(!for_supervisor) > MAX_LAUNCH_EXTRAS {
            return Err(io::const_error!(
                io::ErrorKind::InvalidInput,
                "more connectors provided than a launch carries beside its place",
            ));
        }

        let place = match self.parent {
            Parent::Supervisor => None,
            Parent::Place(place) => Some(toyos_abi::RawHandle(place)),
            Parent::Caller => Some(toyos::AsHandle::as_handle(toyos::endow::this_process())),
        };
        let parent = match place {
            None => launch::Parent::Supervisor,
            Some(place) => launch::Parent::Place(
                toyos_abi::syscall::dup(place).map_err(crate::sys::to_io_error)?,
            ),
        };
        let release = |slots: &[(u32, toyos_abi::RawHandle)]| {
            for (_, h) in slots {
                toyos_abi::syscall::close(*h);
            }
            if let launch::Parent::Place(copy) = parent {
                toyos_abi::syscall::close(copy);
            }
        };

        let mut slots: Vec<(u32, toyos_abi::RawHandle)> = Vec::with_capacity(slot_map.len());
        for &[child_slot, parent] in slot_map {
            match toyos_abi::syscall::dup(toyos_abi::RawHandle(parent)) {
                Ok(copy) => slots.push((child_slot, copy)),
                Err(e) => {
                    release(&slots);
                    return Err(crate::sys::to_io_error(e));
                }
            }
        }
        let extras: Vec<(&str, toyos_abi::RawHandle)> = self
            .provided
            .iter()
            .map(|(name, handle)| (name.as_str(), toyos_abi::RawHandle(*handle)))
            .collect();
        let request = Launch { program, argv, env, cwd, extras: &extras, slots: &slots, parent };
        let mut home = crate::vec![0u8; toyos::ipc::MAX_FRAME_LEN as usize];
        let answer = launch::launch(&conn, &request, &mut home);

        // **The send moved them.** Every arm below but `NotSent` is past the
        // point where these duplicates left this table, so closing them here
        // would be this process naming a handle it does not hold — which the
        // kernel answers by ending it. The launcher releases what it took.
        match answer {
            Ok(Outcome::Started(handle)) => {
                // SAFETY: the supervisor moved this handle into our table and holds none.
                Ok(Routed::Started(Process {
                    handle: unsafe { toyos::process::Process::from_raw(handle) },
                }))
            }
            // The direct path, which is what §4.5 clause 2 says an undeclared
            // program gets. A caller that transferred connectors loses nothing
            // by it: the supervisor merges a launched program's extras into the namespace
            // it builds, so a caller that was itself launched already carries
            // them, and the child inherits that. What the direct path cannot do
            // is merge a name into an *inherited* namespace — no caller in the
            // tree needs it, and
            // `issues/isolation/a-provided-name-cannot-reach-an-undeclared-child.md`
            // is where that is written down.
            // It carries the `HOME` the supervisor decided for the program, which the
            // direct spawn hands on in place of this process's own.
            Ok(Outcome::NotDeclared { home }) => direct(Some(home.into())),
            Ok(Outcome::Refused) | Err(LaunchError::Sent(_)) => {
                Err(io::Error::from(io::ErrorKind::Other))
            }
            Ok(Outcome::Gone) => Err(crate::sys::to_io_error(toyos_abi::syscall::SyscallError::Gone)),
            Err(LaunchError::NotSent(_)) => {
                release(&slots);
                direct(None)
            }
        }
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

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub struct ExitStatus(i32);

impl Default for ExitStatus {
    fn default() -> Self {
        ExitStatus(0)
    }
}

impl ExitStatus {
    pub fn exit_ok(&self) -> Result<(), ExitStatusError> {
        if self.0 == 0 { Ok(()) } else { Err(ExitStatusError(self.0)) }
    }

    pub fn code(&self) -> Option<i32> {
        Some(self.0)
    }
}

impl fmt::Display for ExitStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "exit status: {}", self.0)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ExitStatusError(i32);

impl Into<ExitStatus> for ExitStatusError {
    fn into(self) -> ExitStatus {
        ExitStatus(self.0)
    }
}

impl ExitStatusError {
    pub fn code(self) -> Option<NonZero<i32>> {
        NonZero::new(self.0)
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
            Ok(code) => Ok(Some(ExitStatus(code))),
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
