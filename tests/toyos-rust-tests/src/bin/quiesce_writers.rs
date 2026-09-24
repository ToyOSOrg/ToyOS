//! Writers that never stop, and a reboot underneath them.
//!
//! **The shape the shutdown's two claims are false in.** `quiesce` syncs every
//! filesystem and then says `Rebooting.`; both are claims about a machine, and
//! a machine with threads still writing when they are made is a machine they
//! are not true of. So this boot puts [`WRITERS`] threads into an unbounded
//! write-and-fsync loop, waits for every one of them to say it has finished a
//! pass, and then asks for the reset from a thread that is not one of them.
//!
//! Nothing here asserts: `common::power::quiesce_stops_the_machine` reads the
//! kernel's own `stop:` record and the order of the console around it, which
//! are the two things a guest cannot see about its own death.

use std::fs::File;
use std::io::Write;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::syscap::SysCap;

/// Threads writing when the reset is asked for. More than one CPU's worth on
/// the harness's guest, so the reset cannot simply find them all descheduled.
const WRITERS: usize = 6;

/// Bytes per write: over a page, so each one is a real block-layer operation
/// rather than a page-cache touch.
const CHUNK: usize = 8192;

/// How long the writers get to reach their loop before this boot gives up on
/// being a machine with anything to stop.
///
/// Inside the host's own wait for this guest to stop, so the line below reaches
/// the console it is read from rather than a budget ending the boot first.
const SPIN_UP: Duration = Duration::from_secs(5);

/// What a writer says every pass.
const WRITING: &str = "quiesce-writer:";

fn main() {
    let Some(cap) = Endowments::get().take::<SysCap>(SYSCAP_LABEL) else {
        eprintln!("quiesce_writers: this program was endowed no system capability");
        std::process::exit(1);
    };

    // One word per writer that has finished a pass. **The reset is asked for
    // over a machine every writer is known to be working on**: a writer still
    // being spawned when the last word is written puts no line above it, which
    // is a boot that had nothing to stop.
    let (in_the_loop, first_passes) = mpsc::channel::<usize>();
    for writer in 0..WRITERS {
        let in_the_loop = in_the_loop.clone();
        std::thread::Builder::new()
            .name(format!("writer{writer}"))
            .spawn(move || {
                let path = format!("/log/quiesce-{writer}.bin");
                let payload = [b'q'; CHUNK];
                for pass in 0u64.. {
                    // One per pass, so the rate is the write's and not a spin's.
                    println!("{WRITING} {writer} {pass}");
                    // Reopened each pass: the close is what puts the last
                    // chunk's pages where only a sync can reach them.
                    //
                    // A writer that fails says so and is gone; the harness
                    // counts the threads the kernel stopped, so one fewer is
                    // the red, and this line is its reason.
                    let mut f = match File::create(&path) {
                        Ok(f) => f,
                        Err(e) => {
                            eprintln!("{WRITING} {writer} could not create {path}: {e}");
                            return;
                        }
                    };
                    for _ in 0..8 {
                        if let Err(e) = f.write_all(&payload) {
                            eprintln!("{WRITING} {writer} could not write {path}: {e}");
                            return;
                        }
                    }
                    if let Err(e) = f.sync_all() {
                        eprintln!("{WRITING} {writer} could not sync {path}: {e}");
                        return;
                    }
                    if pass == 0 {
                        in_the_loop.send(writer).expect("main holds the receiver");
                    }
                }
            })
            .expect("spawn a writer");
    }

    let give_up = Instant::now() + SPIN_UP;
    for reached in 0..WRITERS {
        let left = give_up.saturating_duration_since(Instant::now());
        if first_passes.recv_timeout(left).is_err() {
            eprintln!("quiesce_writers: {reached} of {WRITERS} writers reached their loop in {SPIN_UP:?}");
            std::process::exit(1);
        }
    }
    println!("{WRITERS} writers are running; asking for the reset");

    // Comes back only refused: on the other path the machine is already at its
    // firmware, and every writer above is still mid-loop when it goes.
    let refused = cap.reboot();
    eprintln!("quiesce_writers: the reboot was refused ({refused:?})");
    std::process::exit(1);
}
