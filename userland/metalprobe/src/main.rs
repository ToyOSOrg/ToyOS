//! The device measurements a metal boot takes of itself, one command per
//! number, invoked by the name it is symlinked under the way `toybox` is.
//!
//! **The exit code is the measurement, because on the T14 nothing else
//! crosses.** A userland `println!` ends at `Backend::None` on a machine with
//! no serial port, and there is no `SYS_LOG_WRITE`; the one word of a process
//! that reaches the stick is the kernel's own record at its exit,
//! `exit: <name> pid=N code=<code> cpu=Nms` (`kernel/src/process.rs`). That
//! record carries the full `i32` the process exited with, so a command here
//! exits with its measured value in the unit its own module declares, and the
//! process name in the record is the symlink it was invoked under. The host
//! reads both out of the log the stick came back with.
//!
//! **Every command's number is a span in microseconds**, and never a rate. The
//! profile that prices them (`tests/metal-profile.toml`) holds a ceiling per
//! number, and a ceiling is what a duration has: a rate would have to be judged
//! from below, against a floor nothing in that file can express. What each span
//! covers is its own module's to say, and the byte count it covers is a
//! constant there.
//!
//! A command that could not measure exits with one of [`Refusal`]'s negative
//! codes instead, so a missing number is never a small one.

mod fb;
mod usb;

use std::process::exit;

/// Why a command has no number, as the negative exit codes the host reads.
///
/// **Negative, and never zero**: a measurement is a count and a count of zero
/// is a legal answer, so a refusal may not share the space with one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The endowment table holds no device-minting capability.
    NoCapability = -1,
    /// The kernel refused the device claim.
    NoDevice = -2,
    /// The claim described a display this command cannot measure.
    NoScanout = -3,
    /// A filesystem call the measurement rests on was refused.
    NoVolume = -4,
    /// What was read back is not what was written.
    Disagreed = -5,
    /// The measurement ran in no time at all, so its rate is not a number.
    NoDuration = -6,
}

impl Refusal {
    fn code(self) -> i32 {
        self as i32
    }
}

/// One command's answer: a measured value in the command's own unit, or why
/// there is none.
pub type Measured = Result<i32, Refusal>;

/// Every command by the name its symlink gives it. The name is what the
/// kernel's `exit:` record carries, so it is also the name the profile prices.
macro_rules! commands {
    ($($name:literal => $run:path),+ $(,)?) => {
        const COMMANDS: &[(&str, fn() -> Measured)] = &[$(($name, $run)),+];
    };
}

commands! {
    "fbcheck" => fb::check,
    "fbfill" => fb::fill,
    "fbread" => fb::read_back,
    "usbwrite" => usb::write,
    "usbread" => usb::read,
}

fn main() {
    let argv0 = std::env::args().next().unwrap_or_default();
    let invoked = std::path::Path::new(&argv0)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string();
    let Some((_, run)) = COMMANDS.iter().find(|(name, _)| *name == invoked) else {
        // Not a refusal of a measurement: this binary was reached under a name
        // no symlink should have made, which is a broken image and not a
        // machine that answered badly.
        eprintln!("metalprobe: no command is named {invoked:?}");
        exit(i32::MIN);
    };
    match run() {
        Ok(value) => exit(value),
        Err(refusal) => exit(refusal.code()),
    }
}

/// A span in whole microseconds, saturated at [`i32::MAX`] so a span too long
/// for the channel is still a number and not a wrap.
///
/// A span the clock could not tell from zero is refused rather than reported:
/// the profile would price it against a ceiling it can never reach, and a
/// measurement that cannot fail is what that file exists to refuse.
pub fn span(nanos: u128) -> Measured {
    let micros = nanos / 1_000;
    if micros == 0 {
        return Err(Refusal::NoDuration);
    }
    Ok(i32::try_from(micros).unwrap_or(i32::MAX))
}
