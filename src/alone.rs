//! Whether two failure sentences are the same failure — the one decision behind
//! the suite's `ALONE:` line.
//!
//! What differs between two readings of one assertion is a measurement; what
//! differs between two failures is an identity. So a sentence is compared with
//! only what is unmistakably a measurement taken out of it: a number carrying a
//! unit of time or size (`0.303 s`, `1007 ms`, `12 MiB`), and the time in a
//! kernel record's stamp (`[kernel 0.075 cpu0]`, which no two boots write the
//! same). Everything else stays, because it names *which* thing was observed —
//! `slot 1` against `slot 2`, a port, an APIC id, a CPU, an opcode, a register,
//! a count with no unit — and two sentences naming two of them are two
//! observations, which is the larger finding of the two.
//!
//! **The limit that rule carries**: an address is digits with no unit after
//! them, so one assertion printing two addresses reads as two failures.

/// The units that make digits a reading. A number carrying none of them names
/// something rather than measuring it.
const UNITS: &[&str] = &["ns", "us", "µs", "ms", "s", "KiB", "MiB", "GiB", "TiB", "kB", "MB", "GB", "B"];

/// Whether `one` and `other` are the same assertion firing, at possibly
/// different readings.
///
/// Both arguments are headlines — one line, the sentence without the capture.
pub fn same_failure(one: &str, other: &str) -> bool {
    skeleton(one) == skeleton(other)
}

/// `text` with every measurement in it replaced by one `#`, which is what is
/// left of a sentence when its readings are taken out of it.
fn skeleton(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while let Some(c) = text[i..].chars().next() {
        let rest = &text[i..];
        if let Some((time, end)) = stamp(rest) {
            out.push_str(&rest[..time]);
            out.push('#');
            i += end;
        } else if let Some(digits) = reading(rest) {
            out.push('#');
            i += digits;
        } else {
            out.push(c);
            i += c.len_utf8();
        }
    }
    out
}

/// Where the boot time sits inside a kernel record's stamp at the head of
/// `text` — the offsets of `0.075` in `[kernel 0.075 cpu0]`.
///
/// The `cpu` field is what tells a stamp from any other bracketed pair, and it
/// is an identity: which CPU wrote a record is an observation about the record.
fn stamp(text: &str) -> Option<(usize, usize)> {
    let inside = text.strip_prefix('[')?;
    let close = inside.find(']')?;
    let mut fields = inside[..close].split(' ');
    let (writer, time, cpu) = (fields.next()?, fields.next()?, fields.next()?);
    let stamped = !writer.is_empty()
        && !time.is_empty()
        && time.bytes().all(|c| c.is_ascii_digit() || c == b'.')
        && cpu.strip_prefix("cpu").is_some_and(|n| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()))
        && fields.next().is_none();
    let at = 1 + writer.len() + 1;
    stamped.then_some((at, at + time.len()))
}

/// The length of the numeric literal at the head of `text`, if a unit follows
/// it — which is the whole of what makes digits a reading.
fn reading(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    if !bytes.first().is_some_and(u8::is_ascii_digit) {
        return None;
    }
    let mut n = 0;
    while n < bytes.len() && bytes[n].is_ascii_digit() {
        n += 1;
    }
    // The fraction digits behind a `.` with digits on both sides: `0.303` is one
    // reading and not two.
    while bytes.get(n) == Some(&b'.') && bytes.get(n + 1).is_some_and(u8::is_ascii_digit) {
        n += 1;
        while n < bytes.len() && bytes[n].is_ascii_digit() {
            n += 1;
        }
    }
    let after = &text[n..];
    let after = after.strip_prefix(' ').unwrap_or(after);
    // A whole unit and not the head of a longer word: `2 sticks` counts nothing.
    UNITS
        .iter()
        .any(|unit| {
            after
                .strip_prefix(unit)
                .is_some_and(|tail| !tail.starts_with(|c: char| c.is_ascii_alphanumeric()))
        })
        .then_some(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIDE: &str = "the controller started at 0.303 s, past the 0.3 s the ports are held \
                        empty for, so nothing in this boot read a hidden port. The boot has \
                        outgrown the injection window: widen SLOW_CONNECT_NS, not this gate";
    const ALONE: &str = "the controller started at 0.300 s, past the 0.3 s the ports are held \
                         empty for, so nothing in this boot read a hidden port. The boot has \
                         outgrown the injection window: widen SLOW_CONNECT_NS, not this gate";

    #[test]
    fn one_assertion_at_two_measurements_is_one_failure() {
        assert_ne!(WIDE, ALONE, "the two runs did write different sentences");
        assert!(same_failure(WIDE, ALONE), "one assertion at two readings read as two");
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

        let endpoints = "3 endpoint(s) were found Running after the break, want 2";
        let pointer = "input never came back: no pointer event moved by (2560, -1920)";
        assert!(!same_failure(endpoints, pointer));
    }

    /// A verdict that quotes a kernel record carries that record's stamp, and
    /// two boots never stamp the same line the same. The sentence is
    /// `tests/toyos.rs`'s `pci_cap_selftest`, which fires on a verdict that is
    /// short of `14/14` and pastes the record it read.
    #[test]
    fn one_verdict_at_two_kernel_stamps_is_one_failure() {
        let control = "not every crafted capability layout was answered: [kernel 0.075 cpu0] \
                       virtio: pci cap selftest 13/14";
        let mutant = "not every crafted capability layout was answered: [kernel 0.082 cpu0] \
                      virtio: pci cap selftest 13/14";
        assert_ne!(control, mutant);
        assert!(same_failure(control, mutant), "one verdict at two kernel stamps read as two");

        // The count is the finding, so the stamp rule does not reach it: twelve
        // layouts answered and thirteen are two observations.
        let fewer = control.replace("13/14", "12/14");
        assert!(!same_failure(control, &fewer));
        // Nor does it reach the CPU beside the time.
        let elsewhere = control.replace("cpu0", "cpu3");
        assert!(!same_failure(control, &elsewhere));
    }

    /// An index is an identity however it is spelled — welded to its name or
    /// separated from it — because two of them are two objects.
    #[test]
    fn an_index_is_an_identity() {
        assert!(!same_failure(
            "usb-storage: slot 1 transport broke on SCSI 0x35",
            "usb-storage: slot 2 transport broke on SCSI 0x35"
        ));
        assert!(!same_failure("slot 3 holds 12 on the device", "slot 4 holds 12 on the device"));
        assert!(!same_failure("cpu5 last reached one 0.349s ago", "cpu6 last reached one 0.349s ago"));
        assert!(same_failure("cpu5 last reached one 0.349s ago", "cpu5 last reached one 0.712s ago"));
        // An opcode is one too, and so is a count that carries no unit.
        assert!(!same_failure(
            "usb-storage: slot 1 transport broke on SCSI 0x35",
            "usb-storage: slot 1 transport broke on SCSI 0x28"
        ));
        assert!(!same_failure(
            "3 endpoint(s) were found Running after the break, want 2",
            "4 endpoint(s) were found Running after the break, want 2"
        ));
    }

    /// The limit the module header states: an address is digits with no unit
    /// after them, so `tests/common/iommu.rs`'s translation assertions read as
    /// two failures at two addresses rather than one at two readings.
    #[test]
    fn two_addresses_are_two_failures() {
        assert!(!same_failure(
            "the retired scanout at 0xfd000000 was given device address 0x40000000, which still \
             translates to 0x1000 while a holder maps the pages",
            "the retired scanout at 0xfe000000 was given device address 0x40000000, which still \
             translates to 0x1000 while a holder maps the pages"
        ));
    }

    #[test]
    fn a_skeleton_keeps_everything_that_is_not_a_reading() {
        assert_eq!(skeleton("started at 0.303 s"), "started at # s");
        assert_eq!(skeleton("read 1007 ms and 12 MiB"), "read # ms and # MiB");
        assert_eq!(skeleton("cpu6 0.349s"), "cpu6 #s");
        assert_eq!(skeleton("[kernel 0.075 cpu0] slot 1 at 0x35"), "[kernel # cpu0] slot 1 at 0x35");
        assert_eq!(skeleton("moved by (2560, -1920)"), "moved by (2560, -1920)");
        assert_eq!(skeleton("a sentence with no readings"), "a sentence with no readings");
        // A unit is a whole word or it is not one, and a bracketed pair that is
        // not a record's stamp is not one either.
        assert_eq!(skeleton("2 sticks in 4 seconds"), "2 sticks in 4 seconds");
        assert_eq!(skeleton("[budget 0.075 left]"), "[budget 0.075 left]");
    }
}
