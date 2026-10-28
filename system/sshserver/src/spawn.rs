//! How this daemon starts what a client asks for, and lets it go.
//!
//! On ToyOS each connection's programs run in a login session the supervisor
//! opens for it ([`Login`]): launched in their own rows, detached under that
//! session's owner, so they hold no host key and no network right of this
//! daemon's. A channel's end asks its program to quit with a hang-up
//! ([`Ender`]), and the connection's end signs its session out, which asks
//! every program in it and ends none by force. Elsewhere a program has no row
//! and runs as this daemon's child, killed when its channel or connection goes.

use std::process::{Child, Command};
use std::sync::Mutex;

/// The login session one connection's programs run in.
#[cfg(target_os = "toyos")]
pub struct Login {
    launcher: toyos::namespace::Namespace,
    control: toyos::ipc::Connection,
}

/// What this daemon was endowed to open sessions with, taken once.
#[cfg(target_os = "toyos")]
static LOGIN: std::sync::OnceLock<Option<toyos::namespace::Namespace>> = std::sync::OnceLock::new();

#[cfg(target_os = "toyos")]
impl Login {
    /// A session opened for one connection.
    pub fn open() -> Result<Self, String> {
        use toyos_session as wire;
        let login = LOGIN
            .get_or_init(|| toyos::endow::Endowments::get().take(wire::LOGIN_PORT))
            .as_ref()
            .ok_or("this daemon holds no `login` connector")?;
        let (launcher, control) = wire::open(login)?;
        Ok(Self { launcher, control })
    }

    /// `program`, to be launched in its own row in this session, detached
    /// under the session's owner.
    pub fn command(&self, program: &str) -> Command {
        use std::os::toyos::process::CommandExt;
        let mut command = Command::new(program);
        command.independent().detached().launcher(toyos::AsHandle::as_handle(&self.launcher).into());
        command
    }
}

/// The connection is gone: its session signs out, asking what runs in it to
/// quit and waiting for it.
#[cfg(target_os = "toyos")]
impl Drop for Login {
    fn drop(&mut self) {
        let asked = toyos_session::SignOut { force: false }.encode();
        if let Err(e) = self.control.try_send_bytes(toyos_session::MSG_SIGN_OUT, &asked) {
            println!("sshserver: a session was not signed out with its connection: {e:?}");
        }
    }
}

/// How a channel's end lets its program go: a hang-up, asked through a handle
/// that may quit it, which a thread waiting for the program does not hold.
#[cfg(target_os = "toyos")]
pub struct Ender(toyos::process::Process);

#[cfg(target_os = "toyos")]
impl Ender {
    pub fn of(child: &Child) -> Result<Self, String> {
        use toyos::endow::FromHandle;
        let quit = toyos::AsHandle::as_handle(child)
            .try_clone_narrowed(toyos_abi::Rights::MANAGE)
            .map_err(|e| format!("no handle to end it was had ({e:?})"))?;
        Ok(Self(toyos::process::Process::from_handle(quit)))
    }

    /// Ask it to quit with a hang-up, whether or not a thread waits for it.
    pub fn end(&self, _child: &Mutex<Child>) {
        let _ = self.0.quit(toyos::quit::Reason::HangUp);
    }
}

/// Elsewhere, no session: a program is this daemon's child.
#[cfg(not(target_os = "toyos"))]
pub struct Login;

#[cfg(not(target_os = "toyos"))]
impl Login {
    pub fn open() -> Result<Self, String> {
        Ok(Self)
    }

    pub fn command(&self, program: &str) -> Command {
        Command::new(program)
    }
}

/// Elsewhere a channel's end kills its program, nobody being left to read it.
#[cfg(not(target_os = "toyos"))]
pub struct Ender;

#[cfg(not(target_os = "toyos"))]
impl Ender {
    pub fn of(_child: &Child) -> Result<Self, String> {
        Ok(Self)
    }

    /// Kill it and reap it, unless a thread is already waiting for its end.
    pub fn end(&self, child: &Mutex<Child>) {
        if let Ok(mut child) = child.try_lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
