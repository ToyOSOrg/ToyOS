//! `/system/bin/surveyend`: the end of a survey boot.
//!
//! **First, `inspect` has said everything.** The log `logkeeper` serves is
//! read from the boot's first line until the kernel's own record of
//! `inspect`'s exit, which comes after every line `inspect` wrote: a stop
//! before it would hold those lines back from the file.
//!
//! **Then the machine powers off as soon as the kernel will.** The kernel
//! refuses a power-off until `acpiserver` has evaluated `\_S5` and handed it
//! the sleep type, and nothing says when that is; so the supervisor is asked
//! again each [`ASK_EVERY`] while the refusal is that one. Each ask has
//! `logkeeper` make the log whole first, which is the supervisor's sequence.
//!
//! Every wait is bounded by [`BOUND`]. The first one past it is said and the
//! power-off asked for anyway, since ending the boot is this program's job;
//! the second ends this program loudly and leaves the machine to the kernel's
//! boot deadline, which seals its record and resets it, as does any refusal
//! but that one.

use std::io::{BufRead, BufReader, Read};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use toyos::power::{self, Refused, Stop};
use toyos_abi::syscall::SyscallError;

/// A bound and not a measurement; the boot deadline stands behind both waits.
const BOUND: Duration = Duration::from_secs(60);
const ASK_EVERY: Duration = Duration::from_secs(1);

/// The kernel's exit record of the one program this waits on.
const INSPECT_ENDED: &str = "kernel] exit: inspect pid=";

/// `logkeeper`'s pipe as a reader.
struct Log(toyos::Pipe);

impl Read for Log {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf).map_err(|e| std::io::Error::other(format!("{e:?}")))
    }
}

fn main() {
    match inspect_ended() {
        Ok(()) => println!("surveyend: inspect has ended, so the machine powers off"),
        Err(why) => eprintln!("surveyend: {why}; the machine powers off without waiting"),
    }
    let until = Instant::now() + BOUND;
    loop {
        // Comes back only refused: on the other path the power is already off.
        let refused = power::stop(Stop::Shutdown);
        if refused != Refused::Kernel(SyscallError::NotSupported) {
            eprintln!("surveyend: the power-off was refused ({refused:?}); the boot deadline ends this boot");
            std::process::exit(1);
        }
        if Instant::now() >= until {
            eprintln!("surveyend: no \\_S5 reached the kernel in {} s; the boot deadline ends this boot", BOUND.as_secs());
            std::process::exit(1);
        }
        std::thread::sleep(ASK_EVERY);
    }
}

/// Wait until the served log carries [`INSPECT_ENDED`], or say why not.
fn inspect_ended() -> Result<(), String> {
    let served = logkeeper_api::read()?;
    let (seen, ended) = mpsc::channel();
    // Detached: a reader blocked on the pipe ends with this process.
    std::thread::spawn(move || {
        for line in BufReader::new(Log(served.pipe)).lines() {
            match line {
                Ok(line) if line.contains(INSPECT_ENDED) => return seen.send(Ok(())),
                Ok(_) => {}
                Err(e) => return seen.send(Err(format!("the served log broke off ({e})"))),
            }
        }
        seen.send(Err("the served log ended".to_string()))
    });
    ended
        .recv_timeout(BOUND)
        .map_err(|_| format!("the log carried no end of inspect in {} s", BOUND.as_secs()))?
}
