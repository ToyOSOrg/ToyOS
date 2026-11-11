//! What a quit asked this shell, and which child it asks to quit in turn.
//!
//! **A quit reaches what the shell runs, but not a child that lands after
//! it**: the kernel asks or ends each child there when the quit arrives, and
//! one that lands later starts holding the reason unread if this shell had not
//! read it, or none if it had (`toyos_proclife::quit`). An interactive shell's
//! line is quit child by child, so only a `-c` shell, which is quit itself,
//! asks a child it started ([`Asked::landed`]).
//!
//! **Only the holder of [`Asked`] reads the notice**, through the `read` each
//! method takes: a reason is never read and not yet recorded where another
//! thread looks.

use toyos_abi::quit::Reason;

/// What a quit asked this shell.
pub struct Asked {
    /// It runs one `-c` command and ends, which an interrupt of its line ends
    /// once the line has.
    one_command: bool,
    /// It waits for a line now: a `-c` shell from its start.
    waiting: bool,
    /// An interrupt read while it waited.
    interrupted: bool,
    /// A hang-up or a terminate read while it waited.
    ended_by: Option<Reason>,
}

impl Asked {
    pub const fn new() -> Self {
        Self { one_command: false, waiting: false, interrupted: false, ended_by: None }
    }

    /// Make this a `-c` shell, whose command is its line from its start: done
    /// before it listens, so no interrupt finds it waiting for nothing.
    pub fn one_command(&mut self) {
        self.one_command = true;
        self.waiting = true;
    }

    pub fn is_one_command(&self) -> bool {
        self.one_command
    }

    /// Record every reason `read` answers, until it answers none: an interrupt
    /// is noted while the shell waits and is nothing at its prompt; a hang-up
    /// or a terminate is noted while it waits, and otherwise ends it now, with
    /// the code this answers.
    pub fn take(&mut self, mut read: impl FnMut() -> Option<Reason>) -> Option<i32> {
        while let Some(reason) = read() {
            match reason {
                Reason::Interrupt => self.interrupted |= self.waiting,
                _ if !self.waiting => return Some(code_of(reason)),
                _ => self.ended_by = Some(reason),
            }
        }
        None
    }

    /// A child this shell started has landed: whether to ask it to quit,
    /// having taken what the notice holds first. A `-c` shell asks it when an
    /// interrupt reached the shell before this look, which may have come
    /// before the child landed and so not reached it.
    ///
    /// No order of this shell's own steps tells such a child from one the
    /// quit's walk already asked, so a child that listens between its landing
    /// and this look is asked twice by one interrupt.
    pub fn landed(&mut self, read: impl FnMut() -> Option<Reason>) -> bool {
        if !self.one_command {
            return false;
        }
        let ended = self.take(read);
        assert_eq!(ended, None, "a -c shell waits from its start");
        self.interrupted
    }

    /// An interactive shell hands its line to the surface and waits for it.
    pub fn waiting(&mut self) {
        self.waiting = true;
        self.interrupted = false;
    }

    /// The line it waited for has ended with `status`: the code the shell
    /// ends with now, having taken what the notice holds, or `None` to run on.
    /// A hang-up or a terminate read meanwhile ends it, and an interrupt ends
    /// a `-c` shell, with `status`.
    pub fn waited(&mut self, status: i32, read: impl FnMut() -> Option<Reason>) -> Option<i32> {
        if let Some(code) = self.take(read) {
            return Some(code);
        }
        self.waiting = self.one_command;
        if let Some(reason) = self.ended_by {
            return Some(code_of(reason));
        }
        (core::mem::take(&mut self.interrupted) && self.one_command).then_some(status)
    }
}

/// The exit code a quit's reason ends a shell with: 128 plus the signal's
/// number, as a shell reads one.
pub fn code_of(reason: Reason) -> i32 {
    128 + match reason {
        Reason::Interrupt => 2,
        Reason::HangUp => 1,
        Reason::Terminate => 15,
    }
}
