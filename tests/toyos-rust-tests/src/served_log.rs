//! This boot's log as logkeeper serves it on test-runner's `log` port, from
//! its first line. A reader is handed each round only after it is on the
//! stick.

use std::time::{Duration, Instant};

use toyos::poller::{Poller, READABLE};
use toyos::Pipe;
use toyos_abi::syscall::SyscallError;
use toyos_logstream::Lines;

pub struct Log {
    pipe: Pipe,
    poller: Poller,
    lines: Lines,
    chunk: Vec<u8>,
}

impl Log {
    pub fn open() -> Log {
        let pipe = logkeeper_api::read().unwrap_or_else(|why| panic!("test-runner's `log` port: {why}")).pipe;
        Log { pipe, poller: Poller::new(1), lines: Lines::new(), chunk: vec![0u8; 64 * 1024] }
    }

    /// Hand `seen` each line in turn until it has answered `true`, for at most
    /// `bound`; `what` names what it waits for.
    pub fn until(&mut self, what: &str, bound: Duration, mut seen: impl FnMut(&str) -> bool) {
        let by = Instant::now() + bound;
        let mut done = false;
        while !done {
            let left = by
                .checked_duration_since(Instant::now())
                .unwrap_or_else(|| panic!("the log did not show {what} within {bound:?}"));
            match self.pipe.read_nonblock(&mut self.chunk) {
                Ok(0) => panic!("logkeeper closed the log before it showed {what}"),
                Ok(n) => self.lines.push(&self.chunk[..n], |line, _| {
                    let line = std::str::from_utf8(line)
                        .unwrap_or_else(|e| panic!("logkeeper served a line that is not UTF-8 ({e}): {line:?}"));
                    done |= seen(line);
                }),
                Err(SyscallError::WouldBlock) => {
                    self.poller.watch(&self.pipe, READABLE, 0);
                    self.poller.wait(1, left.as_nanos() as u64, |_| {});
                }
                Err(e) => panic!("the log's pipe refused a read: {e:?}"),
            }
        }
    }
}
