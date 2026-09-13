//! Writers that never stop, and a reboot underneath them.
//!
//! **The shape the shutdown's two claims are false in.** `quiesce` syncs every
//! filesystem and then says `Rebooting.`; both are claims about a machine, and
//! a machine with threads still writing when they are made is a machine they
//! are not true of. So this boot puts [`WRITERS`] threads into an unbounded
//! write-and-fsync loop, lets them get into it, and then asks for the reset
//! from a thread that is not one of them.
//!
//! **Each writer says so on the console every pass**, which is what makes the
//! order of that console a judge rather than a formality: on a machine that was
//! not stopped, six threads with a hundred milliseconds of shutdown left to run
//! put lines under the boot's own last word, and on one that was stopped not
//! one of them can. Without those lines the console reads the same either way
//! under QEMU, because nothing else these threads do writes a kernel record.
//!
//! Nothing here asserts: `common::power::quiesce_stops_the_machine` reads the
//! kernel's own `stop:` record and the order of the console afterwards, which
//! are the two things a guest cannot see about its own death.

use std::fs::File;
use std::io::Write;
use std::time::Duration;

use toyos::endow::{Endowments, SYSCAP_LABEL};
use toyos::syscap::SysCap;

/// Threads writing when the reset is asked for. More than one CPU's worth on
/// the harness's guest, so the reset cannot simply find them all descheduled.
const WRITERS: usize = 6;

/// Bytes per write: over a page, so each one is a real block-layer operation
/// rather than a page-cache touch.
const CHUNK: usize = 8192;

/// How long the writers get before the reset is asked for. Long enough that
/// every one of them is inside its loop rather than still being spawned.
const SPIN_UP: Duration = Duration::from_millis(300);

/// What a writer says every pass. Mirrored in `tests/common/power.rs`, which
/// counts these under the boot's last word; nothing links the two crates.
const WRITING: &str = "quiesce-writer:";

fn main() {
    let Some(cap) = Endowments::get().take::<SysCap>(SYSCAP_LABEL) else {
        eprintln!("quiesce_writers: this program was endowed no system capability");
        std::process::exit(1);
    };

    for writer in 0..WRITERS {
        std::thread::Builder::new()
            .name(format!("writer{writer}"))
            .spawn(move || {
                let path = format!("/log/quiesce-{writer}.bin");
                let payload = [b'q'; CHUNK];
                for pass in 0u64.. {
                    // The line the judge counts if it lands after the boot's
                    // last word. One per pass, so the rate is the write's and
                    // not a spin's.
                    println!("{WRITING} {writer} {pass}");
                    // Reopened each pass: the close is what puts the last
                    // chunk's pages where only a sync can reach them.
                    let mut f = match File::create(&path) {
                        Ok(f) => f,
                        // The volume going away under us is the reset
                        // arriving, which is this program's whole purpose.
                        Err(_) => return,
                    };
                    for _ in 0..8 {
                        if f.write_all(&payload).is_err() {
                            return;
                        }
                    }
                    if f.sync_all().is_err() {
                        return;
                    }
                }
            })
            .expect("spawn a writer");
    }

    std::thread::sleep(SPIN_UP);
    println!("{WRITERS} writers are running; asking for the reset");

    // Comes back only refused: on the other path the machine is already at its
    // firmware, and every writer above is still mid-loop when it goes.
    let refused = cap.reboot();
    eprintln!("quiesce_writers: the reboot was refused ({refused:?})");
    std::process::exit(1);
}
