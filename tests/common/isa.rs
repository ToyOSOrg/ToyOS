//! The `isa` claim's rows on the T14: what the kernel said beside each job.
//! Every judge reads the kernel's own records, so the same predicate holds a
//! log however it reached the host.

use super::serial::Serial;

/// The kernel's line under `i8042-withheld`: the premise of every row that
/// needs the claim granted.
const WITHHELD: &str = "i8042: withheld, left unprobed for a claim";

/// How often `needle` is in `log`, which must be `want`.
fn said(log: &Serial, needle: &str, want: usize) -> Result<(), String> {
    match log.text().matches(needle).count() {
        seen if seen == want => Ok(()),
        seen => Err(format!("the kernel said {needle:?} {seen} time(s), want {want}:\n{}", log.text())),
    }
}

/// `test_rs_isa_grant` on a boot whose i8042 the kernel left alone: every
/// access its children make that the kernel must refuse, named by the kernel
/// with the port and whose grant it is not. The unbound holder and the moved
/// claim both die at 0x64, the bound children one port past what they hold by
/// an `in` and by an `out`, the wide one on the port its access spans past the
/// grant, and the process holding nothing at the data port.
pub fn ports(kernel: &Serial) -> Result<(), String> {
    kernel.must_say(WITHHELD)?;
    const NAMED: [(&str, usize); 5] = [
        ("in of 1 byte(s) from port 0x0064", 2),
        ("in of 1 byte(s) from port 0x0061", 1),
        ("out of 1 byte(s) to port 0x0061", 1),
        ("in of 1 byte(s) from port 0x0060", 1),
        ("in of 2 byte(s) from port 0x0060, reaching port 0x0061", 1),
    ];
    for (access, times) in NAMED {
        said(kernel, &format!("{access}, which this process holds no grant for"), times)?;
    }
    kernel.must_say("isa: the i8042's ports are pid")?;
    kernel.must_say("isa: the i8042's ports went back with pid")?;
    Ok(())
}

/// `test_rs_isa_lines` on the same boot: one holder read a first interrupt off
/// its claim, and no later claim did.
pub fn lines(kernel: &Serial) -> Result<(), String> {
    kernel.must_say(WITHHELD)?;
    said(kernel, "isa: the i8042 took its first interrupt", 1)
}

/// `test_rs_isa_straddled` under `isa-claim-straddles-quarantine`: the kernel
/// drove the controller, and its quarantine held, resumed and let go once each,
/// whatever raised the flood again after the grant.
pub fn straddle(kernel: &Serial) -> Result<(), String> {
    // The premise: a controller the kernel never armed has no quarantine.
    kernel.must_say("scanning on, GSI ")?;
    for once in [
        "i8042: the quarantine holds after its first step for a claim",
        "i8042: a claim was answered between the quarantine's steps",
        "i8042: quarantined",
    ] {
        said(kernel, once, 1)?;
    }
    said(kernel, "isa: the i8042 took its first interrupt", 1)
}
