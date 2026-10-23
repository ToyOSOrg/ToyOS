//! How this daemon starts what a client asks for.

use std::process::Command;

/// A command for `program` that runs in that program's own row, not this
/// daemon's: it holds no host key and no network right of sshserver's.
#[cfg(target_os = "toyos")]
pub fn own_row(program: &str) -> Command {
    use std::os::toyos::process::CommandExt;
    let mut command = Command::new(program);
    command.independent();
    command
}

/// Elsewhere a program has no row, and runs as this daemon's child.
#[cfg(not(target_os = "toyos"))]
pub fn own_row(program: &str) -> Command {
    Command::new(program)
}
