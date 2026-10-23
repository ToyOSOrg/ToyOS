//! A program this daemon started, which one thread waits for while another
//! may end it.
//!
//! std's `Child` is waited on through one `&mut`. The connection starts the
//! wait once both output streams reach end of file. `end` lets the program go
//! through its [`Ender`]: on ToyOS it asks it to quit with a hang-up through
//! a handle of its own, whether or not a thread waits; on a host it kills it
//! unless a thread already waits, so there a program that closed both outputs
//! and runs on cannot be ended:
//! `issues/sshserver-cannot-end-a-program-that-closed-its-output.md`.

use std::process::Child;
use std::sync::Mutex;

use crate::connection::EXIT_LOST;
use crate::spawn::Ender;

pub struct Program(Mutex<Child>, Ender);

impl Program {
    /// `child`, whose pipes are taken; or the child back, with why it could
    /// not be ended.
    pub fn new(child: Child) -> Result<Self, (Child, String)> {
        match Ender::of(&child) {
            Ok(ender) => Ok(Self(Mutex::new(child), ender)),
            Err(why) => Err((child, why)),
        }
    }

    /// Its exit status once it ends, or [`EXIT_LOST`] for an end the protocol's
    /// status cannot carry, which the console is told.
    pub fn wait(&self, name: &str) -> u32 {
        let status = self.0.lock().expect("no holder panics").wait();
        match status.map(|status| status.code()) {
            Ok(Some(code @ 0..=255)) => code as u32,
            Ok(code) => {
                println!("sshserver: {name} ended with {code:?}, which is not an exit status this protocol carries");
                EXIT_LOST
            }
            Err(e) => {
                println!("sshserver: cannot wait for {name}: {e}");
                EXIT_LOST
            }
        }
    }

    /// Let it go as its channel or connection ends ([`Ender`]).
    pub fn end(&self) {
        self.1.end(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    /// A program ended is reaped, so it leaves no zombie behind.
    #[test]
    fn a_program_ended_is_reaped() {
        let child = Command::new("sleep").arg("1000").stdin(Stdio::null()).spawn().expect("spawn sleep");
        let program = Program::new(child).map_err(|(_, why)| why).expect("a host's child is ended through itself");
        program.end();
        let status = program.0.lock().unwrap().try_wait().expect("try_wait");
        assert!(status.is_some_and(|s| !s.success()), "the killed program was reaped: {status:?}");
    }
}
