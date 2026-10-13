//! What `/system/bin/ps2server` says of the i8042 it armed, read the same way
//! off a QEMU boot's console and a metal boot's log.

use super::irqcensus::Census;
use super::serial::Serial;

/// The probe's line where the keyboard and a pointer were armed.
pub const ARMED_BOTH: &str = "ps2server: armed, command byte 0x57 — keyboard on, aux on";
/// Where the keyboard was armed and the aux device found absent.
pub const ARMED_KEYBOARD: &str = "ps2server: armed, command byte 0x75 — keyboard on, aux off";
/// The keyboard's reset, which names its `0xAA`'s time after the `0xFA`.
pub const KEYBOARD_RESET: &str = "ps2server: kbd reset, 0xfa ";
const AUX_ARMED: &str = "ps2server: aux reset, 0xaa ";
const AUX_ABSENT: &str = "ps2server: aux absent — its reset went unanswered in 600 ms";

/// `log` says ps2server reset the keyboard and armed it set 2 translated, and
/// armed the aux device or found it absent, each by its line; nothing it said
/// is a refusal; and the kernel's interrupt census has no source of its own
/// for the i8042, which [`Census::parse`] refuses a line carrying.
pub fn armed(log: &Serial, kernel: &Serial) -> Result<(), String> {
    let reset = log.must_say(KEYBOARD_RESET)?;
    if !reset.ends_with("— set 2, translated") {
        return Err(format!("the keyboard's reset line is not the armed one: {reset}"));
    }
    eprintln!("  [ps2server] {}", reset.trim());
    let aux = log
        .must_say(AUX_ARMED)
        .or_else(|_| log.must_say(AUX_ABSENT))
        .map_err(|_| format!("ps2server said neither {AUX_ARMED:?} nor {AUX_ABSENT:?}:\n{}", log.text()))?;
    eprintln!("  [ps2server] {}", aux.trim());
    let armed = if aux.contains(AUX_ARMED) { ARMED_BOTH } else { ARMED_KEYBOARD };
    log.must_say(armed)?;
    for refusal in ["ps2server: kbd DISABLED", "ps2server: aux DISABLED", "ps2server: DISABLED"] {
        log.must_not_say(refusal)?;
    }
    for line in kernel.text().lines() {
        if let Some(Err(why)) = Census::parse(line) {
            return Err(format!("the census line {line:?} is not this kernel's sources: {why}"));
        }
    }
    Ok(())
}
