//! The USB transport break on the T14's boot stick, judged off the stick's own
//! log: `usb_stick_left`'s metal row.

use super::serial;

/// The staged break on a real stick: the transfer abandoned on the boot stick's
/// first WRITE(10) is recovered, the write completes, the disk keeps its
/// number, and the boot goes on to the deliberate reboot that ends its chain.
pub fn transport_break_on_metal(
    kernel: &serial::Serial,
    after: &serial::Serial,
) -> Result<(), String> {
    transport_break_recovered(kernel)?;
    super::power::done_chain(after)
}

/// The kernel log's half of [`transport_break_on_metal`].
///
/// **The ladder enters at the port reset**: the break leaves the stick owed a
/// WRITE's data, across which no class reset may be asked. The stick decides
/// the rest. It answers the rung's TEST UNIT READY on its port, and the write
/// goes out again there; or it leaves its port under the reset — a SuperSpeed
/// stick enumerated on the USB2 half of its receptacle trains on the USB3 half
/// — is held, comes back as the same device, and the write goes out again on
/// it. **Either way no rung takes it offline.**
pub fn transport_break_recovered(kernel: &serial::Serial) -> Result<(), String> {
    let staged = kernel.must_say(
        "transport broke on SCSI 0x2a: a staged break skipped the data phase wait; break 1 of ",
    )?;
    let under_test = broke_on(staged)?;
    let entered = kernel.must_say_after(
        staged,
        &format!("usb-storage: {under_test} is owed the data of the command that broke"),
    )?;
    kernel.must_not_say(&format!("usb-storage: {under_test} is offline"))?;
    if let Ok(left) = kernel.must_say_after(entered, " after this driver reset it; it is held ") {
        let back = kernel.must_say_after(left, " as the same device (USB ")?;
        kernel.must_say_after(
            back,
            "is back, and the operation it was asked went out again on it: it completed",
        )?;
        eprintln!("  [usb] {left}");
        eprintln!("  [usb] {back}");
        return Ok(());
    }
    let took = kernel.must_say_after(entered, &format!("usb-storage: {under_test} the port reset took"))?;
    kernel.must_say_after(took, &format!("usb-storage: {under_test} SCSI 0x2a completed after "))?;
    eprintln!("  [usb] {took}");
    Ok(())
}

/// Which device a `usb-storage: <bdf> slot <n> transport broke …` line is about.
///
/// **Refused rather than widened if the line stops naming one.** A count of
/// broken transports is evidence about a disk, and a machine that boots off USB
/// always has at least two: the answer to "how many times did *this* disk's
/// transport break" is not recoverable from a line that does not say which disk
/// it was, and matching every disk's line instead is how this test came to red
/// on a boot stick's own clean recovery.
fn broke_on(line: &str) -> Result<&str, String> {
    line.split_once("usb-storage: ")
        .and_then(|(_, rest)| rest.split_once(" transport broke"))
        .map(|(who, _)| who)
        .ok_or_else(|| {
            format!("{line:?} does not name the device whose transport broke, so nothing can \
                    count that device's breaks apart from another's")
        })
}

