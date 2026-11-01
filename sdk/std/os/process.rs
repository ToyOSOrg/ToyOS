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
    /// `parent_handle`, which this process keeps: the command takes a
    /// duplicate of it now. For a stream's, a listener's or a datagram
    /// socket's, the command keeps the stream or socket instead, and each
    /// spawn exports a holder of it for its child alone. One of 0 to 2
    /// replaces what the command's stdio would have given; a refusal is the
    /// spawn's.
    #[stable(feature = "toyos_ext", since = "1.0.0")]
    fn inherit_handle(&mut self, child_fd: u32, parent_handle: crate::os::toyos::io::BorrowedHandle<'_>) -> &mut Self;

    /// Give the child descriptor `child_fd` as the exported end `end`, which
    /// `toyos::net`'s `export` made and which this process keeps: the command
    /// takes a duplicate of it now, and the child takes it up as a holder of
    /// the stream or the socket.
    #[stable(feature = "toyos_ext", since = "1.0.0")]
    fn inherit_exported(&mut self, child_fd: u32, end: crate::os::toyos::io::BorrowedHandle<'_>) -> &mut Self;

    /// Give the child a handle under a name it can look itself up by.
    ///
    /// The handle is **moved**: into the command, then by its next spawn into
    /// the child, or closed if that spawn is refused. That is what lets a
    /// capability that admits only one holder, a device claim, be handed over
    /// at all. A parent that wants to keep one duplicates it first.
    #[stable(feature = "toyos_ext", since = "1.0.0")]
    fn endow(&mut self, label: &str, handle: crate::os::toyos::io::OwnedHandle) -> &mut Self;

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
    fn provide(&mut self, name: &str, connector: crate::os::toyos::io::OwnedHandle) -> &mut Self;

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
    fn boot_image(&mut self, image: crate::os::toyos::io::BorrowedHandle<'_>) -> &mut Self;

    /// Place the child under the process `place` names rather than under this
    /// one, so that its end, not this process's, takes the child down.
    ///
    /// `place` is a handle carrying `WRITE` to that process: a copy of the
    /// handle it holds to itself, which it handed on. The command takes a copy
    /// of it now; the spawn is refused `PermissionDenied` for a handle without
    /// `WRITE` or `DUP`, and `BrokenPipe` for a process whose end has begun.
    #[stable(feature = "toyos_ext", since = "1.0.0")]
    fn under(&mut self, place: crate::os::toyos::io::BorrowedHandle<'_>) -> &mut Self;

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
    fn launcher(&mut self, namespace: crate::os::toyos::io::BorrowedHandle<'_>) -> &mut Self;

    /// Start the child as a pager, by `syscap`, a `SysCap` carrying
    /// `Rights::PAGER`, which this process keeps: it may then make the pager
    /// object its file server's pages come from. An independent launch is
    /// refused `InvalidInput`.
    #[stable(feature = "toyos_ext", since = "1.0.0")]
    fn pager(&mut self, syscap: crate::os::toyos::io::BorrowedHandle<'_>) -> &mut Self;
}

#[stable(feature = "toyos_ext", since = "1.0.0")]
impl CommandExt for crate::process::Command {
    fn inherit_handle(&mut self, child_fd: u32, parent_handle: crate::os::toyos::io::BorrowedHandle<'_>) -> &mut Self {
        self.as_inner_mut().inherit_handle(child_fd, parent_handle);
        self
    }

    fn inherit_exported(&mut self, child_fd: u32, end: crate::os::toyos::io::BorrowedHandle<'_>) -> &mut Self {
        self.as_inner_mut().inherit_exported(child_fd, end);
        self
    }

    fn endow(&mut self, label: &str, handle: crate::os::toyos::io::OwnedHandle) -> &mut Self {
        self.as_inner_mut().endow(label, handle);
        self
    }

    fn provide(&mut self, name: &str, connector: crate::os::toyos::io::OwnedHandle) -> &mut Self {
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

    fn boot_image(&mut self, image: crate::os::toyos::io::BorrowedHandle<'_>) -> &mut Self {
        self.as_inner_mut().boot_image(image);
        self
    }

    fn under(&mut self, place: crate::os::toyos::io::BorrowedHandle<'_>) -> &mut Self {
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

    fn launcher(&mut self, namespace: crate::os::toyos::io::BorrowedHandle<'_>) -> &mut Self {
        self.as_inner_mut().launcher(namespace);
        self
    }

    fn pager(&mut self, syscap: crate::os::toyos::io::BorrowedHandle<'_>) -> &mut Self {
        self.as_inner_mut().pager(syscap);
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

/// The child's process handle, for what a caller needs of a process beyond
/// `wait` and `kill`: its accounting, or a narrowed copy to hand on.
#[stable(feature = "toyos_ext", since = "1.0.0")]
impl crate::os::toyos::io::AsHandle for crate::process::Child {
    fn as_handle(&self) -> crate::os::toyos::io::BorrowedHandle<'_> {
        crate::os::toyos::io::BorrowedHandle::from_inner(self.as_inner().handle())
    }
}

#[stable(feature = "toyos_ext", since = "1.0.0")]
impl crate::os::toyos::io::AsRawHandle for crate::process::Child {
    fn as_raw_handle(&self) -> crate::os::toyos::io::RawHandle {
        self.as_inner().handle().raw().0
    }
}

#[stable(feature = "toyos_ext", since = "1.0.0")]
impl crate::os::toyos::io::IntoRawHandle for crate::process::Child {
    fn into_raw_handle(self) -> crate::os::toyos::io::RawHandle {
        self.into_inner().into_handle().into_raw().0
    }
}

#[stable(feature = "toyos_ext", since = "1.0.0")]
impl From<crate::process::Child> for crate::os::toyos::io::OwnedHandle {
    fn from(child: crate::process::Child) -> Self {
        Self::from_inner(child.into_inner().into_handle())
    }
}
