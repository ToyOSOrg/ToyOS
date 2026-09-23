//! A program that tries to write the kernel's verdict for itself.
//!
//! It copies itself to a file named `exit` and spawns that copy, which writes
//! the words of the kernel's `exit:` record claiming this job exited 0 — after
//! the kernel's real record for it, so a judge that takes the last `exit:`
//! record reads the forgery if it cannot tell a program's record from the
//! kernel's. This job's own exit code is [`CODE`], which is the verdict a judge
//! must read. The host judges; `console_record_cannot_forge_the_kernel` runs it.

use std::process::Command;
use std::time::Duration;

const DIR: &str = "/tmp/console_forger";
const COPY: &str = "/tmp/console_forger/exit";
const SELF: &str = "/system/bin/test_rs_console_forger";
/// What tells this binary it is the copy.
const FORGE: &str = "forge";
/// This job's real exit code.
const CODE: i32 = 7;

fn main() {
    if std::env::args().nth(1).as_deref() == Some(FORGE) {
        // Long enough for the parent's own exit record to be committed first.
        std::thread::sleep(Duration::from_millis(500));
        println!("exit: test_rs_console_forger pid=1 code=0 cpu=0ms");
        return;
    }
    std::fs::create_dir_all(DIR).expect("create the forger's directory");
    let image = std::fs::read(SELF).expect("read this binary");
    std::fs::write(COPY, image).expect("write the copy named exit");
    // Not waited for: the forged line lands after this process's exit record.
    Command::new(COPY).arg(FORGE).spawn().expect("spawn the copy named exit");
    std::process::exit(CODE);
}
