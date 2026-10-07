use crate::sys::{AsInner, AsInnerMut, FromInner, IntoInner, process as imp};

/// Create a `Stdio` that pipes through a tty-typed handle.
///
/// Like `Stdio::piped()`, but the pipe endpoints are marked as tty so the
/// child process gets canonical mode (echo + line editing) on its stdin.
#[stable(feature = "toyos_ext", since = "1.0.0")]
pub fn tty_piped() -> crate::process::Stdio {
    crate::process::Stdio::from_inner(imp::Stdio::MakeTtyPipe)
}

/// ToyOS-specific extensions to [`process::Command`].
#[stable(feature = "toyos_ext", since = "1.0.0")]
pub trait CommandExt {
    /// Pass an additional handle to the child process.
    ///
    /// The child process will inherit `parent_handle` at slot `child_slot`.
    /// This is useful for passing pipe handles (e.g., for jobserver
    /// protocols) to child processes.
    #[stable(feature = "toyos_ext", since = "1.0.0")]
    fn inherit_handle(&mut self, child_slot: u32, parent_handle: u32) -> &mut Self;

    /// Give the child a handle under a name it can look itself up by.
    ///
    /// The handle is **moved**: after a successful spawn the parent no longer
    /// holds it, which is what lets a capability that admits only one holder —
    /// a device claim — be handed over at all. A parent that wants to keep one
    /// duplicates it first.
    #[stable(feature = "toyos_ext", since = "1.0.0")]
    fn endow(&mut self, label: &str, handle: u32) -> &mut Self;

    /// Put `connector` in the child's namespace under `name`, on top of what
    /// the manifest says the child holds.
    ///
    /// **This is a launch, not a spawn.** A terminal's `surface` port exists
    /// once per terminal, so the supervisor cannot know it and the manifest cannot
    /// name it — but the shell's own `[programs]` row is what should decide the
    /// rest of what a shell holds. So the caller supplies this one connector,
    /// the supervisor supplies the row, and the child's namespace is the union.
    ///
    /// The connector is **moved**, like [`endow`](CommandExt::endow), and the
    /// spawn fails if this process holds no `launcher` connector: there is
    /// nowhere else the manifest row can come from.
    #[stable(feature = "toyos_ext", since = "1.0.0")]
    fn provide(&mut self, name: &str, connector: u32) -> &mut Self;

    /// Make now, on this thread, every file read this command's spawn needs:
    /// find the program, judge the working directory, and read a program on a
    /// file server into memory. The spawn then calls no file server for them —
    /// for a caller whose spawning thread may not wait on one, as
    /// `/system/bin/supervisor`'s loop may not wait on the file servers it starts
    /// again. A later [`current_dir`](crate::process::Command::current_dir)
    /// undoes it.
    #[stable(feature = "toyos_ext", since = "1.0.0")]
    fn prepare(&mut self) -> crate::io::Result<&mut Self>;

    /// Run the program at `path` rather than the one the command names, which
    /// stays the child's `argv[0]`: the kernel opens `path`, or the spawn reads
    /// it when a file server serves it, and nothing opens `argv[0]`.
    ///
    /// **A spawn, never a launch.** The launcher runs the program its manifest
    /// row names, so with [`under_supervisor`](CommandExt::under_supervisor) or
    /// [`provide`](CommandExt::provide) the spawn answers `PermissionDenied`.
    #[stable(feature = "toyos_ext", since = "1.0.0")]
    fn image_from(&mut self, path: &crate::path::Path) -> &mut Self;

    /// Place the child under the process `place` names rather than under this
    /// one, so that its end, not this process's, takes the child down.
    ///
    /// `place` is a handle carrying `WRITE` to that process: a copy of the
    /// handle it holds to itself, which it handed on. The spawn is refused
    /// `PermissionDenied` for a handle without `WRITE`, or without `DUP` for a
    /// program the supervisor launches, which carries a copy, and `BrokenPipe` for a
    /// process whose end has begun.
    #[stable(feature = "toyos_ext", since = "1.0.0")]
    fn under(&mut self, place: u32) -> &mut Self;

    /// Ask the supervisor to be the child's parent: the one way for a child to outlive
    /// this process.
    ///
    /// **A launch or nothing.** The supervisor starts a program its manifest declares,
    /// holding what its row says, so with no `launcher` connector, for a
    /// program no row declares, or for a command that endows a handle or names
    /// a slot beyond stdio, the spawn answers `PermissionDenied` and starts
    /// nothing.
    #[stable(feature = "toyos_ext", since = "1.0.0")]
    fn under_supervisor(&mut self) -> &mut Self;
}

#[stable(feature = "toyos_ext", since = "1.0.0")]
impl CommandExt for crate::process::Command {
    fn inherit_handle(&mut self, child_slot: u32, parent_handle: u32) -> &mut Self {
        self.as_inner_mut().inherit_handle(child_slot, parent_handle);
        self
    }

    fn endow(&mut self, label: &str, handle: u32) -> &mut Self {
        self.as_inner_mut().endow(label, handle);
        self
    }

    fn provide(&mut self, name: &str, connector: u32) -> &mut Self {
        self.as_inner_mut().provide(name, connector);
        self
    }

    fn prepare(&mut self) -> crate::io::Result<&mut Self> {
        self.as_inner_mut().prepare()?;
        Ok(self)
    }

    fn image_from(&mut self, path: &crate::path::Path) -> &mut Self {
        self.as_inner_mut().image_from(path.as_os_str());
        self
    }

    fn under(&mut self, place: u32) -> &mut Self {
        self.as_inner_mut().under(place);
        self
    }

    fn under_supervisor(&mut self) -> &mut Self {
        self.as_inner_mut().under_supervisor();
        self
    }
}

/// ToyOS-specific extensions to [`process::Child`].
#[stable(feature = "toyos_ext", since = "1.0.0")]
pub trait ChildExt {
    /// Give up this process's handle, for one about to be sent or endowed.
    ///
    /// After this the parent no longer holds the child: it cannot wait for it,
    /// kill it or read its accounting. The supervisor's launcher is the caller —
    /// it answers with the handle and keeps none, because a process that could
    /// ask it to start `/bin/true` in a loop would otherwise exhaust the one
    /// handle table the whole machine depends on.
    #[stable(feature = "toyos_ext", since = "1.0.0")]
    fn into_raw_handle(self) -> u32;

    /// This child's process handle, without giving it up.
    ///
    /// **A number to pass to the ABI, not a second owner.** It is what a caller
    /// wanting more of a process than `wait` and `kill` — its accounting, a
    /// narrowed duplicate to hand on — reaches through, and it stays valid only
    /// while the `Child` is alive. std deliberately does not wrap those calls:
    /// their argument and answer types are `toyos-abi`'s, and a std signature
    /// naming them would drag every caller onto the sysroot's copy of that
    /// crate rather than its own.
    #[stable(feature = "toyos_ext", since = "1.0.0")]
    fn as_raw_handle(&self) -> u32;
}

#[stable(feature = "toyos_ext", since = "1.0.0")]
impl ChildExt for crate::process::Child {
    fn into_raw_handle(self) -> u32 {
        self.into_inner().into_raw_handle()
    }

    fn as_raw_handle(&self) -> u32 {
        self.as_inner().as_raw_handle()
    }
}
