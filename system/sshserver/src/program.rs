//! A program this daemon started, which one thread waits for while another
//! may end it.
//!
//! std's `Child` is waited on and ended through one `&mut`, so the wait
//! starts only once both output pipes are at their end, which the program's
//! own end is what does. Until then the program can be ended. A program that
//! closed both pipes and runs on is waited for and cannot be:
//! `issues/sshserver-cannot-end-a-program-that-closed-its-output.md`.

use std::process::Child;
use std::sync::Mutex;

use crate::connection::EXIT_LOST;

pub struct Program(Mutex<Child>);

impl Program {
    /// `child`, whose pipes are taken.
    pub fn new(child: Child) -> Self {
        Self(Mutex::new(child))
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

    /// End it, unless a thread is already waiting for its end.
    pub fn end(&self) {
        if let Ok(mut child) = self.0.try_lock() {
            let _ = child.kill();
        }
    }
}
