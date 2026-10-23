//! How this daemon starts what a client asks for.
//!
//! On ToyOS each connection's programs run in a login session the supervisor
//! opens for it ([`Login`]): launched in their own rows, detached under that
//! session's owner, so they hold no host key and no network right of this
//! daemon's. A connection that goes signs its session out, which asks its
//! programs to quit and ends none by force. Elsewhere a program has no row
//! and runs as this daemon's child, killed when its connection goes.

use std::process::{Child, Command};

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
        command.independent().detached().launcher(toyos::AsHandle::as_handle(&self.launcher).0);
        command
    }

    /// A program whose connection is gone: its session's sign-out asks it to
    /// quit, so this daemon lets it go.
    pub fn abandon(_child: &mut Child, name: &str, peer: &str) {
        println!("sshserver: {peer}: the connection is gone; {name} is asked to quit with its session");
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

    /// A program whose connection is gone is killed: nobody is left to read it.
    pub fn abandon(child: &mut Child, name: &str, peer: &str) {
        match child.kill().and_then(|()| child.wait()) {
            Ok(_) => println!("sshserver: {peer}: the connection is gone; ended {name}"),
            Err(e) => println!("sshserver: {peer}: the connection is gone and {name} would not end: {e}"),
        }
    }
}
