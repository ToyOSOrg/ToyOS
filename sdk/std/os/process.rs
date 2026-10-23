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
    /// Give the child descriptor `child_fd`, naming the kernel handle
    /// `parent_handle`, which this process keeps: the spawn duplicates it, or,
    /// for a stream's, gives the child a holder of the stream of its own. One
    /// of 0 to 2 replaces what the command's stdio would have given.
    #[stable(feature = "toyos_ext", since = "1.0.0")]
    fn inherit_handle(&mut self, child_fd: u32, parent_handle: u32) -> &mut Self;

    /// Give the child descriptor `child_fd` as the stream end `end`, which
    /// `toyos::net::stream::export` made and which this process keeps: the
    /// spawn duplicates it, and the child takes it up as a holder of the
    /// stream.
    #[stable(feature = "toyos_ext", since = "1.0.0")]
    fn inherit_exported(&mut self, child_fd: u32, end: u32) -> &mut Self;

    /// Give the child a handle under a name it can look itself up by.
    ///
    /// The handle is **moved**: after a successful spawn the parent no longer
    /// holds it, which is what lets a capability that admits only one holder —
    /// a device claim — be handed over at all. A parent that wants to keep one
    /// duplicates it first.
    #[stable(feature = "toyos_ext", since = "1.0.0")]
    fn endow(&mut self, label: &str, handle: u32) -> &mut Self;

    /// Put `connector` in an [`independent`](CommandExt::independent)
    /// child's namespace under `name`, on top of what its manifest row holds.
    ///
    /// A terminal's `surface` port exists once per terminal, so no manifest
    /// can name it, but the shell's own row is what should decide the rest of
    /// what a shell holds. So the caller supplies this one connector, the
    /// launcher supplies the row, and the child's namespace is the union.
    ///
    /// The connector is **moved**, like [`endow`](CommandExt::endow). A
    /// command that is not independent is refused `InvalidInput`.
    #[stable(feature = "toyos_ext", since = "1.0.0")]
    fn provide(&mut self, name: &str, connector: u32) -> &mut Self;

    /// Make now, on this thread, every file read this command's spawn needs:
    /// find the program, judge the working directory, and read a program on a
    /// file server into memory. The spawn then calls no file server for them —
    /// for a caller whose spawning thread may not wait on one, as
    /// the supervisor's loop may not wait on the file servers it starts
    /// again. A later [`current_dir`](crate::process::Command::current_dir)
    /// undoes it but for the program read, which a prepare after it keeps and
    /// a later [`image_from`](CommandExt::image_from) does not.
    #[stable(feature = "toyos_ext", since = "1.0.0")]
    fn prepare(&mut self) -> crate::io::Result<&mut Self>;

    /// The bytes a prepared spawn starts, as [`prepare`](CommandExt::prepare)
    /// read them off a file server: what a caller holds a program to before it
    /// runs, with no second read between the two. `None` before a prepare and
    /// for a program the kernel opens itself.
    #[stable(feature = "toyos_ext", since = "1.0.0")]
    fn prepared_image(&self) -> Option<&[u8]>;

    /// Run the program at `path` rather than the one the command names, which
    /// stays the child's `argv[0]`: the kernel opens `path`, or the spawn reads
    /// it when a file server serves it, and nothing opens `argv[0]`.
    ///
    /// **A helper's alone.** The launcher runs the program its manifest row
    /// names, so an [`independent`](CommandExt::independent) command that
    /// names an image is refused `InvalidInput`.
    #[stable(feature = "toyos_ext", since = "1.0.0")]
    fn image_from(&mut self, path: &crate::path::Path) -> &mut Self;

    /// Run the boot image `image` names, an immutable region this process
    /// holds and keeps, rather than any file: the command's program is the
    /// child's `argv[0]` and nothing opens it. What the supervisor starts the
    /// file server with before any file can be read.
    #[stable(feature = "toyos_ext", since = "1.0.0")]
    fn boot_image(&mut self, image: u32) -> &mut Self;

    /// Place the child under the process `place` names rather than under this
    /// one, so that its end, not this process's, takes the child down.
    ///
    /// `place` is a handle carrying `WRITE` to that process: a copy of the
    /// handle it holds to itself, which it handed on. The spawn is refused
    /// `PermissionDenied` for a handle without `WRITE`, or without `DUP` for an
    /// independent launch, which carries a copy, and `BrokenPipe` for a
    /// process whose end has begun.
    #[stable(feature = "toyos_ext", since = "1.0.0")]
    fn under(&mut self, place: u32) -> &mut Self;

    /// Start the child as an **independent launch** through the session
    /// launcher this process holds: the program its manifest row declares,
    /// holding that row's authority, its own identity and the connectors
    /// [`provide`](CommandExt::provide)d. Without it a spawn is a helper,
    /// which inherits this process's namespace, environment and the
    /// descriptors its spawn names.
    ///
    /// **A launch or nothing.** It is refused `PermissionDenied`, with the
    /// reason, where this process holds no launcher, the launcher will not
    /// open, or no row declares the program or this one may not start it; and
    /// `InvalidInput` for an endowment other than a job, a descriptor past
    /// stdio or an [`image_from`](CommandExt::image_from). It is never
    /// started as a helper instead. A relative program is made absolute
    /// against the child's working directory.
    #[stable(feature = "toyos_ext", since = "1.0.0")]
    fn independent(&mut self) -> &mut Self;

    /// Place an [`independent`](CommandExt::independent) child under the
    /// launcher's owner, so that it outlives this process. A command that is
    /// not independent is refused `InvalidInput`.
    #[stable(feature = "toyos_ext", since = "1.0.0")]
    fn detached(&mut self) -> &mut Self;

    /// Make an [`independent`](CommandExt::independent) launch through the
    /// launcher `namespace` names, a namespace holding `launcher` that this
    /// process holds and keeps, in place of the one it was endowed: a login
    /// authority's for a session it opened. It reaches only what that launcher
    /// does. A command that is not independent is refused `InvalidInput`.
    #[stable(feature = "toyos_ext", since = "1.0.0")]
    fn launcher(&mut self, namespace: u32) -> &mut Self;
}

#[stable(feature = "toyos_ext", since = "1.0.0")]
impl CommandExt for crate::process::Command {
    fn inherit_handle(&mut self, child_fd: u32, parent_handle: u32) -> &mut Self {
        self.as_inner_mut().inherit_handle(child_fd, parent_handle);
        self
    }

    fn inherit_exported(&mut self, child_fd: u32, end: u32) -> &mut Self {
        self.as_inner_mut().inherit_exported(child_fd, end);
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

    fn prepared_image(&self) -> Option<&[u8]> {
        self.as_inner().prepared_image()
    }

    fn image_from(&mut self, path: &crate::path::Path) -> &mut Self {
        self.as_inner_mut().image_from(path.as_os_str());
        self
    }

    fn boot_image(&mut self, image: u32) -> &mut Self {
        self.as_inner_mut().boot_image(image);
        self
    }

    fn under(&mut self, place: u32) -> &mut Self {
        self.as_inner_mut().under(place);
        self
    }

    fn independent(&mut self) -> &mut Self {
        self.as_inner_mut().independent();
        self
    }

    fn detached(&mut self) -> &mut Self {
        self.as_inner_mut().detached();
        self
    }

    fn launcher(&mut self, namespace: u32) -> &mut Self {
        self.as_inner_mut().launcher(namespace);
        self
    }
}

/// ToyOS-specific extensions to [`process::ExitStatus`].
#[stable(feature = "toyos_ext", since = "1.0.0")]
pub trait ExitStatusExt {
    /// The word `SYS_PROCESS_WAIT` answered for this end: an exit, a kill or a
    /// fault, which `toyos_abi::end::End::from_word` reads. std names no type of
    /// that crate.
    #[stable(feature = "toyos_ext", since = "1.0.0")]
    fn into_raw(self) -> u64;

    /// The `<signal.h>` number a kill or a fault ends a process as, as Unix's
    /// `ExitStatusExt::signal` answers: `None` for an exit.
    #[stable(feature = "toyos_ext", since = "1.0.0")]
    fn signal(&self) -> Option<i32>;
}

#[stable(feature = "toyos_ext", since = "1.0.0")]
impl ExitStatusExt for crate::process::ExitStatus {
    fn into_raw(self) -> u64 {
        self.as_inner().into_raw()
    }

    fn signal(&self) -> Option<i32> {
        self.as_inner().signal()
    }
}

/// ToyOS-specific extensions to [`process::Child`].
#[stable(feature = "toyos_ext", since = "1.0.0")]
pub trait ChildExt {
    /// Give up this process's handle, for one about to be sent or endowed.
    ///
    /// After this the parent no longer holds the child: it cannot wait for it,
    /// kill it or read its accounting. The launcher is the caller —
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
