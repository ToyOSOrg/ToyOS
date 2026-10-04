//! What a metal boot says about itself: the account the shutdown seals into
//! the black box.
//!
//! **Text in, verdicts out.** Everything here reads the two strings
//! `src/metal.rs` brings back off the stick — the loader's file and `logkeeper`'s —
//! and nothing here touches a machine.

#![forbid(unsafe_code)]

/// The head of the reset's own account, as `kernel/src/drivers/xhci/stop.rs`
/// spells it.
pub const QUIESCE_HEAD: &str = "usb-quiesce:";

/// What the stop calls the thing it settles before it touches a port. Every
/// reset says this once, whether a device was inside one or not.
pub const QUIESCE_COMMAND: &str = "Bulk-Only command";

/// What the stop says about the endpoint the controller had when it ran — the
/// one line in the account that is the hardware's word and not the driver's.
pub const QUIESCE_ENDPOINT: &str = "the controller had that device's data endpoint";

/// What the shutdown did, out of its summary line.
///
/// **Read as pairs and never as totals**, because "one controller halted" and
/// "one of two controllers halted" are the difference the whole path exists
/// for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quiesced {
    pub flushed: u32,
    pub disks: u32,
    /// Of those disks, how many answered INVALID COMMAND OPERATION CODE to
    /// SYNCHRONIZE CACHE. **Reported and never judged**: a device with no write
    /// cache made nothing durable by refusing, so the flush is `ok` and means a
    /// different thing — and on the T14 this is every disk, which is why the
    /// count is here rather than left to the word `ok`.
    pub cacheless: u32,
    /// Connected ports whose reset finished, of the connected ports there were.
    /// **The act an attached device sees**, and the one that ends a transfer.
    pub reset_ports: u32,
    pub connected: u32,
    pub halted: u32,
    pub controllers: u32,
    pub reset: u32,
    pub unpowered: u32,
    pub ports: u32,
}

impl Quiesced {
    /// Whether every device the boot had was handed back: every disk's cache
    /// emptied, every connected port reset, every controller halted and reset.
    ///
    /// Port *power* is reported and not judged: `PORTSC.PP` is writable only on
    /// a controller with Port Power Control, so a count under the total is a
    /// fact about the silicon and not about the shutdown. The port *reset* is
    /// judged, because it is the one act an attached device sees and no
    /// controller may decline it.
    pub fn complete(&self) -> bool {
        self.flushed == self.disks
            && self.reset_ports == self.connected
            && self.halted == self.controllers
            && self.reset == self.controllers
            && self.controllers > 0
    }
}

/// The shutdown's summary out of `loader.log`, or `None` where the pass carries
/// none.
pub fn quiesced(loader: &str) -> Option<Quiesced> {
    let line = loader.lines().find(|l| l.contains(" disk cache(s) flushed"))?;
    let pair = |what: &str| -> Option<(u32, u32)> {
        let (a, b) = line.split(what).next()?.split_whitespace().next_back()?.split_once('/')?;
        Some((a.parse().ok()?, b.parse().ok()?))
    };
    let (flushed, disks) = pair(" disk cache(s) flushed")?;
    let cacheless: u32 =
        line.split(" with no cache to flush").next()?.split_whitespace().next_back()?.parse().ok()?;
    let (reset_ports, connected) = pair(" connected port(s) reset")?;
    let (halted, controllers) = pair(" controller(s) halted")?;
    let (unpowered, ports) = pair(" port(s) unpowered")?;
    let reset = line.split(" controller(s) halted, ").nth(1)?.split_whitespace().next()?;
    Some(Quiesced {
        flushed,
        disks,
        cacheless,
        reset_ports,
        connected,
        halted,
        controllers,
        reset: reset.parse().ok()?,
        unpowered,
        ports,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `loader.log`'s pass after the reset, with the stop's tail and the
    /// shutdown's own account under the `|` the loader prefixes a report's
    /// lines with.
    fn a_good_loader() -> String {
        "ToyOS Bootloader 1.0\n\
         Black box: the last boot read DONE, so it handed the machine back on purpose and this \
         chain ends here\n\
         | log: this boot's newest records follow, newest first (16)\n\
         | log-tail: [ 3.960 cpu0 kernel] Rebooting.\n\
         | log-tail: [ 3.955 cpu0 kernel] usb-quiesce: disk 0 SYNCHRONIZE CACHE ok\n\
         | usb-quiesce: no Bulk-Only command was open, so this reset cuts none\n\
         | usb-quiesce: xHCI 00:14.0 halted=true USBSTS=0x00000009\n\
         | usb-quiesce: 2/2 disk cache(s) flushed, 1 with no cache to flush, \
         5/5 connected port(s) reset, 2/2 controller(s) halted, 2 reset, \
         12/16 port(s) unpowered\n\
         Loader log: the last boot is accounted for, so this pass resets the machine\n"
            .to_string()
    }

    #[test]
    fn the_shutdowns_own_account_is_read_out_of_the_loaders_file() {
        assert_eq!(
            quiesced(&a_good_loader()),
            Some(Quiesced {
                flushed: 2,
                disks: 2,
                cacheless: 1,
                reset_ports: 5,
                connected: 5,
                halted: 2,
                controllers: 2,
                reset: 2,
                unpowered: 12,
                ports: 16,
            })
        );
        assert!(quiesced(&a_good_loader()).unwrap().complete());
        // Ports are reported, not judged: a controller with no Port Power
        // Control ignores the write and 0/16 is the silicon's answer.
        assert!(quiesced(&a_good_loader().replace("12/16 port", "0/16 port")).unwrap().complete());
        // Each of the four that *are* judged, failing on its own.
        for (from, to) in [
            ("2/2 disk", "1/2 disk"),
            ("5/5 connected", "4/5 connected"),
            ("2/2 controller(s) halted", "1/2 controller(s) halted"),
            ("halted, 2 reset", "halted, 1 reset"),
            // A shutdown that found no controller is not one that handed
            // everything back; it is a machine this table is not about.
            ("2/2 controller(s) halted, 2 reset", "0/0 controller(s) halted, 0 reset"),
        ] {
            let moved = a_good_loader().replace(from, to);
            assert!(!quiesced(&moved).unwrap().complete(), "{from} -> {to}");
        }
        assert_eq!(quiesced("ToyOS Bootloader 1.0\n"), None);
    }

    /// **The lines this crate judges a reset by are the kernel's own**, and
    /// nothing links the two: a reword at the site that did not come here would
    /// leave every metal readback passing on a predicate that matches nothing.
    #[test]
    fn the_kernel_writes_the_quiesce_lines_the_host_reads() {
        let at = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("kernel/src/drivers/xhci/stop.rs");
        let source = std::fs::read_to_string(&at).expect("the reset path");
        let written = code_of(&source);
        for needle in [QUIESCE_HEAD, QUIESCE_COMMAND, QUIESCE_ENDPOINT] {
            assert!(written.contains(needle), "{} does not write {needle:?}", at.display());
        }
    }

    /// A line the kernel only *talks* about is not a line the kernel writes:
    /// every needle above appears in that file's own prose as well, so a scan
    /// over the raw source passes on a kernel that emits none of them.
    #[test]
    fn a_needle_that_only_appears_in_a_comment_is_not_found() {
        assert_eq!(code_of("    //! a Bulk-Only command was open\n"), "");
        assert_eq!(code_of("    /// no Bulk-Only command was open\n"), "");
        assert_eq!(code_of("    // no Bulk-Only command was open\n"), "");
        assert!(code_of("    writeln!(said, \"no Bulk-Only command was open\");\n")
            .contains(QUIESCE_COMMAND));
    }

    /// `source` with its whole-line comments taken out, which is every form
    /// this tree's prose takes: a doc comment, a module header, a note above a
    /// statement. A trailing comment after code is left, and cannot carry one
    /// of these needles without the code above it on the same line.
    fn code_of(source: &str) -> String {
        source
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}
