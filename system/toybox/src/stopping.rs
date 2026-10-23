//! `shutdown` and `reboot`: a stop asked of the supervisor, which has the log
//! made whole first ([`toyos::power`]). **The `power` connector is the whole
//! of the authority**: each is `/system/bin/toybox` under another name,
//! holding what `[programs.toybox]` declares.
//!
//! The stop waits for every session to end; `--force` ends what stays. What
//! it waits for is printed as it changes, until the machine stops or the stop
//! is cancelled.

use toyos::power::{self, Mode, Refused, Stop, Update};

pub fn main(name: &str, how: Stop, args: &[String]) {
    let mode = match args {
        [] => Mode::Wait,
        [force] if force == "--force" => Mode::Terminate,
        _ => {
            eprintln!("usage: {name} [--force]");
            std::process::exit(2);
        }
    };
    let mut pending = match power::stop(how, mode) {
        Ok(pending) => pending,
        Err(Refused::NotEndowed) => refused(&format!("{name}: this process holds no `power` connector; run it with `launch {name}`")),
        Err(why) => refused(&format!("{name}: refused ({why:?})")),
    };
    waiting(name, &pending.waiting.names);
    if !pending.watched {
        println!("{name}: the stop is under way; the supervisor tells this no more");
        return;
    }
    // A stop that happens ends this process with the machine.
    loop {
        match pending.read() {
            Update::Waiting(now) => waiting(name, &now.names),
            Update::Cancelled => refused(&format!("{name}: the stop was cancelled")),
            Update::Refused(why) => refused(&format!("{name}: the machine did not stop ({why:?})")),
            Update::Gone => refused(&format!("{name}: the supervisor hung up")),
        }
    }
}

fn waiting(name: &str, names: &[String]) {
    if !names.is_empty() {
        println!("{name}: waiting for {}; `{name} --force` ends them", names.join(", "));
    }
}

fn refused(why: &str) -> ! {
    eprintln!("{why}");
    std::process::exit(1);
}
