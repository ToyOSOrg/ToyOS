//! The C corpus's comparator, in the guest.
//!
//! **Every case in the corpus `return 0`s unconditionally**, so an exit-code
//! verdict would be vacuous: what a case is judged on is its whole output
//! against a committed `.expect`, and on a machine with no serial port that
//! output reaches nothing a host can read. So the comparison happens here, and
//! what crosses is this program's exit code — which the kernel writes as
//! `exit: <name> pid=N code=N`.
//!
//! **The name is the case, and that is the whole trick.** One binary is staged
//! once and reached through one symlink per case, so the kernel records each
//! run under the case's own name and a host reading the stick can say which of
//! a hundred and nineteen failed. `argv[0]` is what this reads to know which
//! one it is.
//!
//! It is also a *better* comparison than the host's. The host reads a console
//! every process on the machine shares, and has to take the other writers'
//! lines out before comparing (`common::console::c_verdict`); this reads one
//! pipe that only the case can write to, so there is nothing to filter and no
//! line that can be attributed wrongly.

use std::io::Read;
use std::process::{Command, Stdio};

/// Where a case's committed expectation is staged, and where its binary is.
const EXPECT: &str = "/system/expect";
const CASE: &str = "/system/bin/test_c_";

/// What this program's exit code means. **The only channel it has**, so each
/// number is a different thing that went wrong rather than a shade of "no".
mod code {
    pub const MATCHED: i32 = 0;
    pub const DIFFERED: i32 = 1;
    /// The case itself exited non-zero, which no case in this corpus does.
    pub const CASE_FAILED: i32 = 2;
    /// No expectation was staged for this case, so nothing was compared. A
    /// pass here would be a case that is never judged again.
    pub const NO_EXPECTATION: i32 = 3;
    /// The case would not start, which is a staging fault and not a verdict.
    pub const NO_CASE: i32 = 4;
    /// This program was reached under a name it cannot turn into a case.
    pub const NO_NAME: i32 = 5;
}

fn main() {
    let code = run();
    // Said as well as returned: under QEMU a console reads it, and the exit
    // code is what a stick carries.
    println!("ccheck: {code}");
    std::process::exit(code);
}

fn run() -> i32 {
    let Some(argv0) = std::env::args().next() else { return code::NO_NAME };
    let case = argv0.rsplit('/').next().unwrap_or(&argv0);
    if case.is_empty() || case == "ccheck" {
        eprintln!("ccheck: reached as {argv0:?}, which names no case");
        return code::NO_NAME;
    }

    let Ok(expected) = std::fs::read(format!("{EXPECT}/{case}")) else {
        eprintln!("ccheck: {case}: no expectation at {EXPECT}/{case}");
        return code::NO_EXPECTATION;
    };

    let mut child = match Command::new(format!("{CASE}{case}"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            eprintln!("ccheck: {case}: {CASE}{case} would not start: {e}");
            return code::NO_CASE;
        }
    };
    let mut got = Vec::new();
    if let Some(mut out) = child.stdout.take() {
        if let Err(e) = out.read_to_end(&mut got) {
            eprintln!("ccheck: {case}: reading the case's output: {e}");
            return code::NO_CASE;
        }
    }
    let status = match child.wait() {
        Ok(status) => status,
        Err(e) => {
            eprintln!("ccheck: {case}: waiting for the case: {e}");
            return code::NO_CASE;
        }
    };
    if status.code() != Some(0) {
        eprintln!("ccheck: {case}: the case exited {:?}", status.code());
        return code::CASE_FAILED;
    }
    // **The host's rule, and it is one line there too.**
    // `tests/common/console.rs`'s `verdict` compares `mine.trim_end()` against
    // `expected.trim_end()`, so a case that ends its output with a newline and
    // an expectation that does not are the same answer — six of the corpus's
    // cases are exactly that pair, in one direction or the other. Spelled here
    // because a guest binary cannot link the harness, and held to the host's
    // by `the_two_comparisons_use_one_rule`.
    let got = trim_end(&got);
    let expected = trim_end(&expected);
    if got != expected {
        // Both, and the first byte they part at: on a stick none of this
        // reaches anything, but under QEMU it is what a reader needs and the
        // exit code alone never says *where*.
        let at = got
            .iter()
            .zip(expected)
            .position(|(a, b)| a != b)
            .unwrap_or(got.len().min(expected.len()));
        eprintln!(
            "ccheck: {case}: differed at byte {at} — {} byte(s) produced against {} expected",
            got.len(),
            expected.len()
        );
        eprintln!("ccheck: {case}: got      {:?}", String::from_utf8_lossy(got));
        eprintln!("ccheck: {case}: expected {:?}", String::from_utf8_lossy(expected));
        return code::DIFFERED;
    }
    code::MATCHED
}

/// The whole normalisation, and it is the host's: trailing whitespace on either
/// side is not a difference.
fn trim_end(bytes: &[u8]) -> &[u8] {
    let mut end = bytes.len();
    while end > 0 && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    &bytes[..end]
}
