//! Whether two failure sentences are the same failure — the one decision behind
//! the suite's `ALONE:` line.
//!
//! **The failure this exists to stop.** The classifier compared the wide run's
//! headline with the alone run's byte for byte, and an assertion that prints
//! what it measured writes a different sentence every time it fires. Nightly
//! `ci` run 35072262489 is the case: one assertion, two boots, `0.303 s` and
//! `0.300 s`, reported as `red again on a DIFFERENT failure — it failed twice,
//! on two assertions`. That line is a *larger* finding than a reproduction, so
//! the defect sends an adjudicator looking for a second defect that does not
//! exist, and it does it to every timing, counting or sizing assertion in the
//! suite.
//!
//! **What counts as a measurement.** A numeric literal standing on its own:
//! `0.303`, `2560`, `94.1`. A digit welded to a name is not one — `cpu6`,
//! `0x35`, `smp8`, `IR0` — because the thing that differs there is *which* CPU,
//! opcode or register, and two sentences naming two of them are two
//! observations. The rule is the character before the digits: a letter or digit
//! makes it part of a name, anything else makes it a reading.

/// Whether `one` and `other` are the same assertion firing, at possibly
/// different readings.
///
/// Both arguments are headlines — one line, the sentence without the capture.
pub fn same_failure(one: &str, other: &str) -> bool {
    one == other || skeleton(one) == skeleton(other)
}

/// `text` with every standalone numeric literal replaced by one `#`, which is
/// what is left of a sentence when its readings are taken out of it.
fn skeleton(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        let welded = i > 0 && bytes[i - 1].is_ascii_alphanumeric();
        if !bytes[i].is_ascii_digit() || welded {
            out.push(bytes[i] as char);
            i += 1;
            continue;
        }
        // The literal, and the fraction digits behind any `.` that has digits
        // on both sides — `0.303` is one reading and not `#.#`.
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        while i + 1 < bytes.len() && bytes[i] == b'.' && bytes[i + 1].is_ascii_digit() {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
        }
        out.push('#');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two sentences the defect was found on, quoted from the run.
    ///
    /// `tests/toyos.rs` holds the gate on the *line* the classifier prints; this
    /// is the gate on the decision under it, which is the part that was wrong.
    const WIDE: &str = "the controller started at 0.303 s, past the 0.3 s the ports are held \
                        empty for, so nothing in this boot read a hidden port. The boot has \
                        outgrown the injection window: widen SLOW_CONNECT_NS, not this gate";
    const ALONE: &str = "the controller started at 0.300 s, past the 0.3 s the ports are held \
                         empty for, so nothing in this boot read a hidden port. The boot has \
                         outgrown the injection window: widen SLOW_CONNECT_NS, not this gate";

    #[test]
    fn one_assertion_at_two_measurements_is_one_failure() {
        assert_ne!(WIDE, ALONE, "the two runs did write different sentences");
        assert!(
            same_failure(WIDE, ALONE),
            "nightly 35072262489's two boots of xhci_slow_connect read as two assertions"
        );
        assert!(same_failure(WIDE, WIDE), "a sentence is itself");
    }

    /// Two assertions of the *same* test, which is the pair a looser rule would
    /// collapse: both open `the first port was named` and both carry two
    /// readings, and they are different findings.
    #[test]
    fn two_assertions_stay_two_failures() {
        let floor = "the first port was named 0.004 s after the controller started, inside the \
                     0.4 s the held-empty window and the debounce behind it come to — the \
                     injection did not reach the driver";
        let ceiling = "the first port was named 0.980 s after the ports were powered, 0.580 s \
                       after the connect became visible — the settle did not end on the device \
                       appearing";
        assert!(!same_failure(floor, ceiling), "two assertions read as one reproduced defect");

        // The pair the `ALONE` line was written for: `xhci_hid_break`'s endpoint
        // count against its pointer delivery, on run 31424496450.
        let endpoints = "3 endpoint(s) were found Running after the break, want 2";
        let pointer = "input never came back: no pointer event moved by (2560, -1920)";
        assert!(!same_failure(endpoints, pointer));
    }

    /// A verdict that quotes a kernel record carries that record's stamp, and
    /// two boots never stamp the same line the same. The pair PR #443's fourth
    /// review met on `pci_capability_walk`: the `[kernel 0.075 cpu0]` is the
    /// stamp the run printed, the second stamp is staged, and the sentence
    /// around them is `tests/toyos.rs`'s `pci_cap_selftest` word for word.
    #[test]
    fn one_verdict_at_two_kernel_stamps_is_one_failure() {
        let control = "not every crafted capability layout was answered: [kernel 0.075 cpu0] \
                       virtio: pci cap selftest 14/14";
        let mutant = "not every crafted capability layout was answered: [kernel 0.082 cpu0] \
                      virtio: pci cap selftest 14/14";
        assert_ne!(control, mutant);
        assert!(same_failure(control, mutant), "one verdict at two kernel stamps read as two");
    }

    /// A digit inside a name is not a reading: `kernel_heartbeat`'s probe names
    /// the CPU that went quiet, and two CPUs are two observations.
    #[test]
    fn a_name_that_ends_in_a_digit_is_not_a_measurement() {
        assert!(!same_failure(
            "cpu5 last reached one 0.349s ago",
            "cpu6 last reached one 0.349s ago"
        ));
        assert!(same_failure(
            "cpu5 last reached one 0.349s ago",
            "cpu5 last reached one 0.712s ago"
        ));
        assert!(!same_failure(
            "usb-storage: slot 1 transport broke on SCSI 0x35",
            "usb-storage: slot 1 transport broke on SCSI 0x28"
        ));
    }

    #[test]
    fn a_skeleton_keeps_everything_that_is_not_a_reading() {
        assert_eq!(skeleton("started at 0.303 s"), "started at # s");
        assert_eq!(skeleton("cpu6 0.349s"), "cpu6 #s");
        assert_eq!(skeleton("moved by (2560, -1920)"), "moved by (#, -#)");
        assert_eq!(skeleton("a sentence with no readings"), "a sentence with no readings");
        // A trailing dot is punctuation and stays; only digits on both sides of
        // one make it part of the reading.
        assert_eq!(skeleton("it took 12."), "it took #.");
    }
}
