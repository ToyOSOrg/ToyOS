//! Launch an installed package, holding nothing that could start it otherwise.
//!
//! This binary is no `[programs]` key, so it holds what `test-runner` holds,
//! and `tests/pkgcase/system.toml` gives that estate a `launcher` connector and
//! no `compositor` one. A window that appears after this came from the `[apps]`
//! row init built out of `/apps/gbae/manifest.toml`, because inheritance
//! carries nothing that would draw one.
//!
//! It does not wait: gbae runs until the machine goes down, and the compositor
//! census on the host's side of the serial says the window exists.

use std::process::Command;

const PROGRAM: &str = "/apps/gbae/gbae";

fn main() -> std::process::ExitCode {
    // The refusal arm is a first-class outcome and not a panic: the same
    // binary runs before the package is installed and after it is removed,
    // where init answering "no" is the assertion.
    match Command::new(PROGRAM).spawn() {
        Ok(child) => {
            println!("pkg-launch: started {PROGRAM} as pid {}", child.id());
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            println!("pkg-launch: {PROGRAM} did not start: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
