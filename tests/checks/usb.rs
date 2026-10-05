use super::*;
use serial::Serial;

/// The metal verdict on the boot stick's staged transport break, off the
/// records the T14 wrote when its stick left the USB2 half of its receptacle
/// under the port rung's reset, beside the shapes on either side of it.
pub fn transport_break_verdict() -> Result<(), String> {
    let judged = |what: &str, log: &str, green: bool| {
        match (usb::transport_break_recovered(&Serial::named(what, log)), green) {
            (Ok(()), true) | (Err(_), false) => Ok(()),
            (Ok(()), false) => Err(format!("{what} passed a log it has to refuse:\n{log}")),
            (Err(why), true) => Err(format!("{what} refused a log it has to pass: {why}\n{log}")),
        }
    };
    const STAGED: &str = "\
        [ 1.174 cpu3 kernel] usb-storage: 00:14.0 slot 1 transport broke on SCSI 0x2a: a staged \
        break skipped the data phase wait; break 1 of 3 running\n";
    const OWED: &str = "\
        [ 1.174 cpu3 kernel] usb-storage: 00:14.0 slot 1 is owed the data of the command that \
        broke, so nothing can be asked of it on the Bulk-Out: its port is reset with no class \
        reset before it\n";
    const RESET: &str = "\
        [ 1.229 cpu3 kernel] xHCI: 00:14.0 slot 1 port 1 reset while recovering (hot on a USB2 \
        port): PORTSC 0x00000e03 then 0x00200e03, link Active, speed 3: reset, and the port is \
        enabled\n";
    const UNANSWERED: &str = "\
        [ 1.279 cpu3 kernel] xHCI: Address Device (after the port reset) failed: code 4 (USB \
        Transaction Error)\n";
    const CLIMBED_ON: &str = "\
        [ 1.279 cpu3 kernel] usb-storage: 00:14.0 slot 1 the port reset was not answered; break 2 \
        of 3 running\n\
        [ 1.279 cpu3 kernel] usb-storage: 00:14.0 slot 1 broke 2 times running; its port reset did \
        not bring the transport back\n\
        [ 1.279 cpu3 kernel] xHCI: 00:14.0 slot 1 port 1 reset while taking it offline (hot on a \
        USB2 port): PORTSC 0x000202a0 then 0x000202a0, link RxDetect, speed 0: nothing is \
        connected, so its port's teardown takes it from here\n\
        [ 1.279 cpu3 kernel] usb-storage: 00:14.0 slot 1 is offline: both bulk endpoints \
        Stopped=true, port 1 reset=false and nothing sent after it, Reset Device=false, its slot \
        goes back to the controller; every operation on it is refused from here\n";
    const LEFT: &str = "\
        [ 1.279 cpu3 kernel] usb-storage: 00:14.0 slot 1 the port reset was not answered, and port \
        1 no longer holds the device (PORTSC 0x000202a0): its port's teardown takes it from here\n";
    const BACK: &str = "\
        [ 1.279 cpu3 kernel] usb-storage: disk 0 left port 1 (its port read empty) after this \
        driver reset it; it is held 1894 ms for the same device to come back\n\
        [ 2.211 cpu0 kernel] usb-storage: disk 0 came back on port 13 slot 6 as the same device \
        (USB 0781:5581, serial number \"FEDCBA98765432FEDCBA\", 7507812 blocks of 512 B), \
        msc_block +0x30000; its volume carries on\n\
        [ 2.259 cpu3 kernel] usb-storage: disk 0 is back, and the operation it was asked went out \
        again on it: it completed\n";
    const TOOK: &str = "\
        [ 1.330 cpu3 kernel] usb-storage: 00:14.0 slot 1 the port reset took: addressed and \
        configured again, the device answered TEST UNIT READY under its own tag 0x5a2\n";
    const COMPLETED: &str = "\
        [ 1.331 cpu3 kernel] usb-storage: 00:14.0 slot 1 SCSI 0x2a completed after 1 break(s) \
        running; the transport came back and the count is cleared\n";
    const CLASS_RESET_TOOK: &str = "\
        [ 1.175 cpu3 kernel] usb-storage: 00:14.0 slot 1 Reset Recovery took: the device answered \
        TEST UNIT READY under its own tag 0x5a2\n";
    const CLASS_RESET_UNANSWERED: &str = "\
        [ 1.175 cpu3 kernel] usb-storage: 00:14.0 slot 1 the class reset was not answered; break 2 \
        of 3 running\n";

    judged("the break as the T14 read it", &format!("{STAGED}{OWED}{RESET}{UNANSWERED}{CLIMBED_ON}{BACK}"), false)?;
    judged("the break its stick left", &format!("{STAGED}{OWED}{RESET}{UNANSWERED}{LEFT}{BACK}"), true)?;
    let lost = BACK.replace(
        "came back on port 13 slot 6 as the same device",
        "did not come back within 2000 ms of its port reset; it is offline",
    );
    judged("a stick that never came back", &format!("{STAGED}{OWED}{RESET}{UNANSWERED}{LEFT}{lost}"), false)?;
    let failed = BACK.replace("it completed", "it failed");
    judged(
        "a write that failed on the stick that came back",
        &format!("{STAGED}{OWED}{RESET}{UNANSWERED}{LEFT}{failed}"),
        false,
    )?;
    judged("a stick that answered on its port", &format!("{STAGED}{OWED}{RESET}{TOOK}{COMPLETED}"), true)?;
    judged(
        "a stick owed a write's data that the class reset brought back",
        &format!("{STAGED}{CLASS_RESET_TOOK}{COMPLETED}"),
        false,
    )?;
    judged(
        "a stick owed a write's data given the class reset before its port reset moved it",
        &format!("{STAGED}{CLASS_RESET_UNANSWERED}{RESET}{UNANSWERED}{LEFT}{BACK}"),
        false,
    )?;
    Ok(())
}
