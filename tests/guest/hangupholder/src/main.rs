//! An app that ignores a hang-up: it reads each reason it is asked to quit,
//! says so where it still can, and stays until it is killed. What it says
//! goes to whatever started it, which a sign-out may already have ended, so
//! a write that fails is no reason to end.

use std::io::Write;

fn main() {
    let notice = toyos::quit::notice().expect("hangupholder: every process holds its notice");
    notice.listen().expect("hangupholder: its notice listens");
    let _ = writeln!(std::io::stdout(), "hangupholder: holding");
    loop {
        let reason = notice.read().expect("hangupholder: its notice reads");
        let _ = writeln!(std::io::stdout(), "hangupholder: asked to quit ({reason:?}), and staying");
    }
}
