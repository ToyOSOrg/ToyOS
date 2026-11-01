//! The claimed-function judges over the T14's own records: the recorded boot
//! whose re-claim spent a second remapping entry, and whose domains reached
//! into a root-bridge window, reds; the same boot with one entry and a capped
//! window passes.

use super::*;

/// The T14's records of one boot, verbatim but for the readback's `stream|`
/// prefix and the declaration's record, which names its declarer as the
/// kernel now does: the I219 enumerated, the root-bridge windows, a domain,
/// and the I219 claimed, released and claimed again.
const RECORDED: &str = r"[2026-09-29 11:54:39  0.065 cpu0 kernel]   PCI 00:1f.6 [0200] vendor=8086 device=15fc prog_if=00 bars=[bar0=0xbcf00000]
[2026-09-29 11:54:39  0.149 cpu0 kernel] pcidev: the acpi claim declared root bridge memory: mem 0xa2000000..0xbd000000, mem 0x4000000000..0x603dc00000, mem 0xa0800000..0xa2000000, mem 0xbd000000..0xc0000000, mem 0xff000000..0xffb80000, mem 0xffd3a070..0x100000000, mem 0x603dc00000..0x8000000000
[2026-09-29 11:54:39  0.250 cpu0 kernel] iommu: domain2 root=0xa31000 aw=48 mgaw=39 addresses from 0x2000000000 to 0x8000000000
[2026-09-29 11:54:39  0.251 cpu0 kernel] iommu: domain2 maps 0x1c00000..0x1e00000 at 0x3fffe00000
[2026-09-29 11:54:39  0.252 cpu0 kernel] iommu: domain2 maps 0x1e00000..0x2200000 at 0x4000000000
[2026-09-29 12:10:34  1.191 cpu0 kernel] iommu: irte5 source=00:1f.6 p=1 sid=0x00fe svt=1 sq=0 vector=0x28 apic=0x0 dst=0x0 trigger=edge
[2026-09-29 12:10:34  1.192 cpu0 kernel] pcidev: PCI 00:1f.6 [8086:15fc] handed over on slot 0, vector 0x28
[2026-09-29 12:10:53 20.233 cpu1 kernel] pcidev: PCI 00:1f.6 [8086:15fc] released from slot 0; reset by nothing (Express: no capability; AF: no capability; PM: No_Soft_Reset set), so where it may still be aimed is kept for its next claim
[2026-09-29 12:10:53 20.238 cpu0 kernel] iommu: irte6 source=00:1f.6 p=1 sid=0x00fe svt=1 sq=0 vector=0x28 apic=0x0 dst=0x0 trigger=edge
[2026-09-29 12:10:53 20.239 cpu0 kernel] pcidev: PCI 00:1f.6 [8086:15fc] handed over on slot 0, vector 0x28
";

/// The second release, which that boot never reached.
const RELEASED: &str = "[2026-09-29 12:10:55 22.000 cpu1 kernel] pcidev: PCI 00:1f.6 [8086:15fc] released from slot 0; reset by nothing\n";

fn log(text: &str) -> serial::Serial {
    serial::Serial::named("T14 log", text)
}

/// The first claim's record, which the re-claim job's spawn precedes.
const FIRST_CLAIM: &str = "[2026-09-29 12:10:34  1.191 cpu0 kernel] iommu: irte";

/// `records` with its claims inside the re-claim job's spawn and exit, and
/// `after` past the exit: what another job on the same boot did.
fn in_the_job(records: &str, after: &str) -> String {
    let spawn = "[2026-09-29 12:10:34  1.190 cpu3 kernel] spawn: /system/bin/test_rs_pci_reclaim pid=7 (layout=0ms tls=0ms total=0ms)\n";
    let exit = "[2026-09-29 12:10:55 22.001 cpu1 kernel] exit: test_rs_pci_reclaim pid=7 code=0 cpu=1ms\n";
    format!("{}{exit}{after}", records.replacen(FIRST_CLAIM, &format!("{spawn}{FIRST_CLAIM}"), 1))
}

/// A third claim of the same function, and its release, by the job after the
/// re-claim: what the shared boot's MSI job does.
const ANOTHER_JOBS_CLAIM: &str = "[2026-09-29 12:10:56 23.000 cpu0 kernel] iommu: irte0 source=00:1f.6 p=1 sid=0x00fe svt=1 sq=0 vector=0x28 apic=0x0 dst=0x0 trigger=edge
[2026-09-29 12:10:56 23.001 cpu0 kernel] pcidev: PCI 00:1f.6 [8086:15fc] handed over on slot 0, vector 0x28
[2026-09-29 12:11:06 33.000 cpu0 kernel] iommu: irte0 source=00:1f.6 p=0 released
[2026-09-29 12:11:06 33.000 cpu0 kernel] pcidev: PCI 00:1f.6 [8086:15fc] released from slot 0; reset by nothing
";

/// One entry for both claims, not present after each release, whatever
/// another job on the boot claims after it; the recorded second entry, and a
/// release that leaves its entry present, red.
pub fn one_entry_per_slot() {
    let cleared = "[2026-09-29 12:10:53 20.234 cpu1 kernel] iommu: irte0 source=00:1f.6 p=0 released\n";
    let green = in_the_job(
        &format!("{RECORDED}{cleared}{RELEASED}{cleared}")
            .replace("irte5 ", "irte0 ")
            .replace("irte6 ", "irte0 "),
        ANOTHER_JOBS_CLAIM,
    );
    assert_eq!(claims::reuses_its_entry(&log(&green)), Ok(()));

    let no_exit = green.replace("pid=7 code=0", "pid=8 code=0");
    let why = claims::reuses_its_entry(&log(&no_exit)).expect_err("a job that never exited");
    assert!(why.contains("never exited"), "{why}");

    let recorded = in_the_job(&format!("{RECORDED}{RELEASED}"), "");
    let why = claims::reuses_its_entry(&log(&recorded)).expect_err("the recorded second entry");
    assert!(why.contains(r#"wrote remapping entries ["5", "6"]"#), "{why}");

    let present = green.replace("p=0 released", "p=1 released");
    let why = claims::reuses_its_entry(&log(&present)).expect_err("an entry left present");
    assert!(why.contains(r#"left (entry, p) [("0", "1"), ("0", "1")]"#), "{why}");
}

/// A mapping reaching into `0x4000000000..` reds; the same mapping placed
/// below the window, and one placed again there, pass.
pub fn domains_map_clear_of_the_windows() {
    let why = claims::clear_of_host_bridges(&log(RECORDED)).expect_err("the mapping inside a window");
    assert!(why.contains("inside the root-bridge window 0x4000000000..0x603dc00000"), "{why}");
    let clear = RECORDED.replace("at 0x4000000000", "at 0x200000 again");
    assert_eq!(claims::clear_of_host_bridges(&log(&clear)), Ok(()));
    // One that starts below the window and ends inside it.
    let straddles = RECORDED.replace("at 0x4000000000", "at 0x3fffe00000");
    assert!(claims::clear_of_host_bridges(&log(&straddles)).is_err());
}
