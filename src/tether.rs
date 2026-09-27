//! A host process the harness has to end cannot outlive the harness, however
//! the harness ends — `SIGKILL` included, which runs no `Drop`.
//!
//! [`spawn`] makes the child the controlling process of a pseudo-terminal whose
//! master only the spawner holds. The kernel closes the master when the
//! spawner dies, by any signal, and a terminal whose master closes is hung up:
//! its controlling process gets `SIGHUP`, on Linux and on macOS.
//! `PR_SET_PDEATHSIG` is Linux's alone and follows the spawning thread rather
//! than the process, and QEMU's `exit-with-parent` is that on Linux and ends
//! QEMU alone.
//!
//! A process that ends on its own — a build, a one-shot client — is not
//! spawned here: a build ended mid-way can leave a toolchain half-written.

use std::io::{self, BufRead, BufReader, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

/// The master: dropping it hangs up the child's terminal.
pub struct Tether {
    _master: OwnedFd,
}

/// Spawn `cmd` as the controlling process of a terminal the returned
/// [`Tether`] holds.
pub fn spawn(mut cmd: Command) -> io::Result<(Child, Tether)> {
    let flags = libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC;
    // SAFETY: a NUL-terminated path, and the descriptor is checked before it is owned.
    let master = unsafe {
        let fd = libc::open(c"/dev/ptmx".as_ptr(), flags);
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        OwnedFd::from_raw_fd(fd)
    };
    // SAFETY: `master` is an open pseudo-terminal master.
    if unsafe { libc::grantpt(master.as_raw_fd()) != 0 || libc::unlockpt(master.as_raw_fd()) != 0 } {
        return Err(io::Error::last_os_error());
    }
    let slave = peer(&master, flags)?;
    let fd = slave.as_raw_fd();
    // SAFETY: system calls on the child's own state, between `fork` and `exec`,
    // allocating nothing.
    unsafe {
        cmd.pre_exec(move || {
            // A mask and a disposition both survive `exec`, and a child that
            // installs no handler of its own would ignore a blocked or ignored
            // hangup.
            let mut hup: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut hup);
            libc::sigaddset(&mut hup, libc::SIGHUP);
            if libc::sigprocmask(libc::SIG_UNBLOCK, &hup, std::ptr::null_mut()) != 0
                || libc::signal(libc::SIGHUP, libc::SIG_DFL) == libc::SIG_ERR
                || libc::setsid() < 0
                || libc::ioctl(fd, libc::TIOCSCTTY as _, 0) < 0
                // Open across `exec`: macOS hangs up only a terminal somebody
                // holds open.
                || libc::fcntl(fd, libc::F_SETFD, 0) < 0
            {
                return Err(io::Error::last_os_error());
            }
            // No hangup precedes the terminal becoming this child's: the child
            // holds its own copy of the master until `exec` closes it.
            Ok(())
        });
    }
    let child = cmd.spawn()?;
    Ok((child, Tether { _master: master }))
}

/// The slave of `master`, opened with `flags`.
#[cfg(target_os = "linux")]
fn peer(master: &OwnedFd, flags: libc::c_int) -> io::Result<OwnedFd> {
    // SAFETY: `master` is an unlocked pseudo-terminal master.
    let fd = unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCGPTPEER, flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a descriptor the ioctl just opened.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// The slave of `master`, opened with `flags`.
#[cfg(target_os = "macos")]
fn peer(master: &OwnedFd, flags: libc::c_int) -> io::Result<OwnedFd> {
    let mut name = [0 as libc::c_char; 128];
    // SAFETY: `TIOCPTYGNAME` writes a NUL-terminated name of at most 128 bytes.
    let fd = unsafe {
        if libc::ioctl(master.as_raw_fd(), libc::TIOCPTYGNAME as _, name.as_mut_ptr()) < 0 {
            return Err(io::Error::last_os_error());
        }
        libc::open(name.as_ptr(), flags)
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a descriptor `open` just returned.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// How long a tethered child may take to exit once its owner is gone: far
/// above a hangup's exit, and a child that outlives its owner never ends.
pub const WITHIN: Duration = Duration::from_secs(10);

/// An owner whose tethered children a test watches die with it.
pub struct Owner {
    child: Child,
    said: BufReader<ChildStdout>,
    /// The owner's stderr, read to its end and sent here: its end is every
    /// process that holds it exited, the owner's children among them.
    closed: Receiver<String>,
}

impl Owner {
    /// Spawn `cmd` as an owner: its stdin a pipe only this process writes, so
    /// it ends when this process does; its stdout read by [`Self::said`]; and
    /// `SIGHUP` blocked and ignored, the worst a harness can hand down to what
    /// it spawns.
    pub fn spawn(mut cmd: Command) -> Result<Owner, String> {
        let (mut stderr, write) = io::pipe().map_err(|e| format!("a pipe for the owner's stderr: {e}"))?;
        cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(write);
        // SAFETY: two system calls on the child's own state, allocating nothing.
        unsafe {
            cmd.pre_exec(|| {
                let mut hup: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&mut hup);
                libc::sigaddset(&mut hup, libc::SIGHUP);
                if libc::sigprocmask(libc::SIG_BLOCK, &hup, std::ptr::null_mut()) != 0
                    || libc::signal(libc::SIGHUP, libc::SIG_IGN) == libc::SIG_ERR
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = cmd.spawn().map_err(|e| format!("spawn the owner: {e}"))?;
        // Its copy of the write end, which would otherwise be a holder too.
        drop(cmd);
        let (tx, closed) = mpsc::channel();
        std::thread::spawn(move || {
            let mut text = Vec::new();
            let _ = stderr.read_to_end(&mut text);
            let _ = tx.send(String::from_utf8_lossy(&text).into_owned());
        });
        let said = BufReader::new(child.stdout.take().expect("a piped stdout"));
        Ok(Owner { child, said, closed })
    }

    /// The rest of the first line the owner prints on stdout that starts with
    /// `prefix`.
    pub fn said(&mut self, prefix: &str) -> Result<String, String> {
        let mut line = String::new();
        loop {
            line.clear();
            match self.said.read_line(&mut line) {
                Ok(0) => {
                    let stderr = self.closed.recv_timeout(WITHIN).unwrap_or_default();
                    return Err(format!("the owner ended without saying {prefix:?}:\n{stderr}"));
                }
                Ok(_) => {}
                Err(e) => return Err(format!("read the owner's stdout: {e}")),
            }
            if let Some(rest) = line.strip_prefix(prefix) {
                return Ok(rest.trim_end().to_string());
            }
        }
    }

    /// `SIGKILL` the owner, and how long every process holding its stderr took
    /// to exit after it. `Err` if one outlived it by [`WITHIN`], naming which
    /// of `pids` still run, and ending them.
    pub fn killed(mut self, pids: &[u32]) -> Result<Duration, String> {
        let killed = Instant::now();
        self.child.kill().map_err(|e| format!("SIGKILL the owner: {e}"))?;
        // Held past the verdict: `wait` would close it, and a child reading
        // it would end on that instead of on its tether.
        let _stdin = self.child.stdin.take();
        self.child.wait().map_err(|e| format!("reap the owner: {e}"))?;
        if self.closed.recv_timeout(WITHIN).is_ok() {
            return Ok(killed.elapsed());
        }
        // SAFETY: signal 0 asks whether the pid exists and delivers nothing.
        let alive: Vec<u32> =
            pids.iter().copied().filter(|&pid| unsafe { libc::kill(pid as i32, 0) } == 0).collect();
        for &pid in &alive {
            // SAFETY: a process of this test's own that still holds its pid.
            unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        }
        Err(format!(
            "a process holding the owner's stderr still ran {WITHIN:?} after the owner's SIGKILL; \
             of its tethered children {pids:?}, {alive:?} did, and were killed"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TETHERED: &str = "tethered ";

    /// This test binary, running the one test `name`.
    fn this_test(name: &str) -> Command {
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args(["--exact", name, "--include-ignored", "--nocapture"]);
        cmd
    }

    #[test]
    #[ignore = "the owner `a_tethered_child_dies_with_its_owner` kills; never runs on its own"]
    fn owner() {
        let mut parked = this_test("tether::tests::parked");
        parked.stdout(Stdio::null());
        let (child, _tether) = spawn(parked).expect("spawn the tethered child");
        println!("{TETHERED}{}", child.id());
        io::stdin().read_to_end(&mut Vec::new()).expect("read the owner's stdin");
    }

    /// A child that never ends on its own while the test runs: its stdin is
    /// the test's pipe, which the owner's death does not close.
    #[test]
    #[ignore = "the tethered child of `owner`; never runs on its own"]
    fn parked() {
        io::stdin().read_to_end(&mut Vec::new()).expect("read the parked child's stdin");
    }

    /// The owner's `SIGKILL` ends its tethered child, which inherited `SIGHUP`
    /// blocked and ignored.
    #[test]
    fn a_tethered_child_dies_with_its_owner() {
        let mut owner = Owner::spawn(this_test("tether::tests::owner")).unwrap_or_else(|e| panic!("{e}"));
        let pid: u32 = owner.said(TETHERED).unwrap_or_else(|e| panic!("{e}")).parse().expect("a pid");
        let took = owner.killed(&[pid]).unwrap_or_else(|e| panic!("{e}"));
        eprintln!("tethered child {pid} gone {took:?} after its owner's SIGKILL");
    }
}
