//! The claimed-function judges over the T14's own records: the recorded boot
//! whose re-claim spent a second remapping entry, and whose domains reached
//! into a root-bridge window, reds; the same boot with one entry and a capped
//! window passes.

use super::*;

/// The T14's records of one boot, verbatim but for the readback's `stream|`
/// prefix: the I219 enumerated, firmware's root-bridge windows, a domain, and
/// the I219 claimed, released and claimed again.
const RECORDED: &str = r"[2026-09-29 11:54:39  0.065 cpu0 kernel]   PCI 00:1f.6 [0200] vendor=8086 device=15fc prog_if=00 bars=[bar0=0xbcf00000]
[2026-09-29 11:54:39  0.149 cpu0 kernel] pcidev: firmware declared root bridge memory: mem 0xa2000000..0xbd000000, mem 0x4000000000..0x603dc00000, mem 0xa0800000..0xa2000000, mem 0xbd000000..0xc0000000, mem 0xff000000..0xffb80000, mem 0xffd3a070..0x100000000, mem 0x603dc00000..0x8000000000
[2026-09-29 11:54:39  0.250 cpu0 kernel] iommu: domain2 root=0xa31000 aw=48 mgaw=39 addresses from 0x2000000000 to 0x8000000000
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

/// One entry for both claims, not present after each release; the recorded
/// second entry, and a release that leaves its entry present, red.
pub fn one_entry_per_slot() {
    let cleared = "[2026-09-29 12:10:53 20.234 cpu1 kernel] iommu: irte0 source=00:1f.6 p=0 released\n";
    let green = format!("{RECORDED}{cleared}{RELEASED}{cleared}")
        .replace("irte5 ", "irte0 ")
        .replace("irte6 ", "irte0 ");
    assert_eq!(claims::reuses_its_entry(&log(&green)), Ok(()));

    let recorded = format!("{RECORDED}{RELEASED}");
    let why = claims::reuses_its_entry(&log(&recorded)).expect_err("the recorded second entry");
    assert!(why.contains(r#"wrote remapping entries ["5", "6"]"#), "{why}");

    let present = green.replace("p=0 released", "p=1 released");
    let why = claims::reuses_its_entry(&log(&present)).expect_err("an entry left present");
    assert!(why.contains(r#"left (entry, p) [("0", "1"), ("0", "1")]"#), "{why}");
}

/// The recorded domain reaches into `0x4000000000..`; capped there, it passes.
pub fn domains_end_below_the_windows() {
    let why = claims::clear_of_host_bridges(&log(RECORDED)).expect_err("the recorded domain");
    assert!(why.contains("inside the root-bridge window 0x4000000000..0x603dc00000"), "{why}");
    let capped = RECORDED.replace("to 0x8000000000", "to 0x4000000000");
    assert_eq!(claims::clear_of_host_bridges(&log(&capped)), Ok(()));
}
