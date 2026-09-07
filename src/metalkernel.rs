//! The kernel suite's profile: what the T14's own kernel must say about the
//! machine on every boot, and what every number that boot measured may be.
//!
//! **Text in, verdicts out.** Everything here reads the string `src/metal.rs`
//! brings back off the stick — `logd`'s file — and nothing here touches a
//! machine. `tests/latencycase` is the boot that produces it: a job list that
//! ends itself, which is the only shape a T14 boot can have.
//!
//! It shares [`Bound`], [`Outcome`] and [`Verdict`] with
//! [`crate::metaldevices`] rather than restating them, because the ceiling
//! discipline is one rule: a number never taken on the machine is
//! [`Bound::Unmeasured`] and is reported rather than judged, the first boot's
//! number becomes its bound with room, and a boot outside it is a red.
//!
//! **What is here that a device profile has no room for** is the arithmetic:
//! several of these records carry two numbers whose *relation* is the
//! assertion, and the relation holds on any machine at any speed. The physical
//! memory manager's three parts sum to what the firmware called usable; every
//! AP the roster committed read a TSC inside the BSP's bracket; every reserved
//! region the DMAR describes falls inside the identity domain; every CPU that
//! came up was checked against the control-register declaration. None of those
//! is a ceiling and none of them is ever `Unmeasured`.

#![forbid(unsafe_code)]

use crate::metaldevices::{exit_of, Bound, Outcome, Presence, Record, Verdict};

use Presence::{NeverSays, Says};

/// The records a kernel-suite boot must carry, whatever the numbers are.
///
/// Every needle is a prefix of a record the **shipping** kernel writes, so
/// nothing in this list needs an actuator: these are facts a machine states
/// about itself on every boot it takes.
pub const RECORDS: &[Record] = &[
    Record { about: "acpi", needle: "ACPI: RSDP at ", presence: Says },
    Record { about: "acpi-inventory", needle: " tables checksummed under the RSDP", presence: Says },
    Record { about: "pmm", needle: "pmm: the firmware map calls ", presence: Says },
    Record { about: "clock", needle: "clock: TSC measured ", presence: Says },
    Record { about: "lapic", needle: "LAPIC timer: ", presence: Says },
    Record { about: "pci", needle: "PCI: Enumeration complete, ", presence: Says },
    Record { about: "smp", needle: " MADT cpus online, ", presence: Says },
    Record { about: "control-regs", needle: "control_regs: ", presence: Says },
    Record { about: "dmar", needle: "iommu: DMAR describes ", presence: Says },
    Record { about: "boot", needle: "Boot: complete (", presence: Says },
    // A CPU that read a TSC outside the BSP's bracket, an AP that never
    // started, and a reserved region the identity domain does not cover — each
    // spelled by the kernel in a word the count above also answers, so a boot
    // that broke one says it twice.
    Record { about: "tsc-trail", needle: "trails the BSP's", presence: NeverSays },
    Record { about: "tsc-lead", needle: "leads the BSP's", presence: NeverSays },
    Record { about: "ap-failed", needle: "failed to start!", presence: NeverSays },
    Record { about: "rmrr-outside", needle: "OUTSIDE the identity domain", presence: NeverSays },
    // The IOMMU's own fault path. A single fault ends the boot, so its absence
    // across a whole boot is what "fault-free" means here.
    Record { about: "dma-fault", needle: "iommu: fault", presence: NeverSays },
];

/// One number a boot's own records carry, read as the word between two others
/// on the first line holding both.
#[derive(Debug, Clone, Copy)]
pub struct Figure {
    pub about: &'static str,
    pub head: &'static str,
    pub tail: &'static str,
    pub unit: &'static str,
    pub bound: Bound,
}

/// Every number this profile holds, and what it is held to.
///
/// **The machine's own identity comes first and is `Exactly` the moment it has
/// been read once**: eight CPUs, a fixed set of ACPI tables, a fixed count of
/// PCI functions. A boot where one of those moved is a machine that changed —
/// firmware, a card, a setting — and that is exactly the thing a bench must not
/// let past silently. The rates and latencies below them are `AtMost`, and both
/// stay [`Bound::Unmeasured`] until the T14 has answered once.
pub const FIGURES: &[Figure] = &[
    Figure {
        about: "cpus",
        head: "SMP: ",
        tail: " of ",
        unit: "cpus online",
        bound: Bound::Unmeasured,
    },
    Figure {
        about: "acpi-tables",
        head: "ACPI: ",
        tail: " of ",
        unit: "tables checksummed",
        bound: Bound::Unmeasured,
    },
    Figure {
        about: "pci-functions",
        head: "PCI: Enumeration complete, ",
        tail: " functions",
        unit: "functions",
        bound: Bound::Unmeasured,
    },
    Figure {
        about: "pmm-managed",
        head: " managed=",
        tail: " ",
        unit: "bytes",
        bound: Bound::Unmeasured,
    },
    Figure {
        about: "tsc-hz",
        head: "clock: TSC measured ",
        tail: "Hz against the HPET",
        unit: "Hz",
        bound: Bound::Unmeasured,
    },
    Figure {
        about: "lapic-hz",
        head: "ticks/10ms, so ",
        tail: "Hz",
        unit: "Hz",
        bound: Bound::Unmeasured,
    },
    // The one cross-source figure a boot has: the HPET-calibrated TSC against
    // what CPUID 15H/16H state. A part that states neither leaf carries no such
    // record, which is why this figure is absent rather than zero there.
    Figure {
        about: "tsc-ppm",
        head: "Hz, ",
        tail: "ppm apart",
        unit: "ppm from CPUID's own figure",
        bound: Bound::Unmeasured,
    },
    Figure {
        about: "boot-ms",
        head: "Boot: complete (",
        tail: "ms)",
        unit: "ms to boot",
        bound: Bound::Unmeasured,
    },
];

/// The TLB bench's record, whose percentiles share one line and so cannot be
/// read by [`Figure`]'s first-line-holding-both rule. How many shootdowns it
/// issued is the kernel's constant and is read off the line rather than
/// restated here.
pub const TLB_BENCH: &str = "tlb: bench ";

/// The percentiles the bench line carries, in the order it writes them.
pub const TLB_PERCENTILES: &[(&str, &str, Bound)] = &[
    ("tlb-min", "min=", Bound::Unmeasured),
    ("tlb-p50", "p50=", Bound::Unmeasured),
    ("tlb-p90", "p90=", Bound::Unmeasured),
    ("tlb-p99", "p99=", Bound::Unmeasured),
    ("tlb-max", "max=", Bound::Unmeasured),
];

/// The jobs `tests/latencycase`'s runner list names, by the name the kernel's
/// `exit:` record spells them with.
///
/// **`cyclictest`'s exit code is its p99 in microseconds, not a verdict.** The
/// contract is in that binary's own module header, and it exists because on the
/// T14 a userland `println!` ends at `Backend::None`: the exit record is the
/// only word a program gets onto the log partition.
pub const CYCLICTEST: &str = "test_rs_cyclictest";
pub const CYCLICTEST_BOUND: Bound = Bound::Unmeasured;
/// What that binary exits with when it measured nothing at all.
pub const CYCLICTEST_FAILED: i64 = 255;

/// The scheduler stress suite, whose exit code *is* a verdict: it asserts and
/// exits zero.
pub const SCHED_STRESS: &str = "test_rs_sched_stress";

/// The relations that hold on any machine at any speed, checked against the
/// records rather than against each other's restatement.
fn arithmetic(log: &str) -> Vec<Verdict> {
    let mut out = Vec::new();
    let mut say = |about: &str, outcome| out.push(Verdict { about: about.to_string(), outcome });

    // The PMM's three parts against the firmware's total.
    match (
        number(log, "the firmware map calls ", " bytes usable"),
        number(log, " managed=", " "),
        number(log, " withheld=", " "),
        number(log, " unaligned=", ","),
    ) {
        (Some(firmware), Some(managed), Some(withheld), Some(unaligned)) => {
            let sum = managed + withheld + unaligned;
            say(
                "pmm-balances",
                if sum == firmware {
                    Outcome::Held(format!("{managed} + {withheld} + {unaligned} = {firmware}"))
                } else {
                    Outcome::Failed(format!(
                        "the firmware map calls {firmware} bytes usable and the PMM accounts for \
                         {sum}"
                    ))
                },
            );
        }
        _ => say("pmm-balances", Outcome::Failed("the PMM's accounting record is unreadable".into())),
    }

    // Every AP the roster committed, inside the BSP's TSC bracket.
    match (number(log, "SMP: ", " of "), number(log, "MADT cpus online, ", " of ")) {
        (Some(online), Some(bracketed)) => say(
            "tsc-bracket",
            if bracketed == online - 1 {
                Outcome::Held(format!("{bracketed} of {} APs inside the BSP's", online - 1))
            } else {
                Outcome::Failed(format!(
                    "{online} CPUs came up and {bracketed} of {} APs read a TSC inside the BSP's \
                     bracket",
                    online - 1
                ))
            },
        ),
        _ => say("tsc-bracket", Outcome::Failed("the SMP roster record is unreadable".into())),
    }

    // Every CPU that came up, checked against the control-register declaration.
    match (number(log, "SMP: ", " of "), number(log, "control_regs: ", " of ")) {
        (Some(online), Some(checked)) => say(
            "control-regs-count",
            if checked == online {
                Outcome::Held(format!("{checked} cpus hold the declaration"))
            } else {
                Outcome::Failed(format!(
                    "{checked} CPUs were checked against the declaration and {online} came up"
                ))
            },
        ),
        _ => say("control-regs-count", Outcome::Failed("the roster or the check is unreadable".into())),
    }

    // Every reserved region the DMAR describes, inside the identity domain.
    match (
        number(log, "units and ", " reserved regions"),
        number(log, " reserved regions, ", " of them inside"),
    ) {
        (Some(regions), Some(held)) => say(
            "rmrr-held",
            if held == regions {
                Outcome::Held(format!("{held} of {regions} inside the identity domain"))
            } else {
                Outcome::Failed(format!(
                    "the DMAR describes {regions} reserved regions and {held} are inside the \
                     identity domain"
                ))
            },
        ),
        _ => say("rmrr-held", Outcome::Failed("the DMAR summary is unreadable".into())),
    }

    out
}

/// Judge one boot's log against this profile.
pub fn judge(log: &str) -> Vec<Verdict> {
    let mut out = Vec::new();
    for record in RECORDS {
        let saw = log.contains(record.needle);
        let outcome = match (record.presence, saw) {
            (Says, true) => Outcome::Held(quoted(log, record.needle)),
            (Says, false) => Outcome::Failed(format!("no record carries {:?}", record.needle)),
            (NeverSays, false) => Outcome::Held(format!("nothing said {:?}", record.needle)),
            (NeverSays, true) => Outcome::Failed(format!(
                "a record carries {:?}: {}",
                record.needle,
                quoted(log, record.needle)
            )),
        };
        out.push(Verdict { about: record.about.to_string(), outcome });
    }

    out.extend(arithmetic(log));

    for figure in FIGURES {
        let outcome = match number(log, figure.head, figure.tail) {
            Some(value) => figure.bound.check(value, figure.unit),
            None => Outcome::Failed(format!(
                "no record carries {:?} and then {:?}",
                figure.head, figure.tail
            )),
        };
        out.push(Verdict { about: figure.about.to_string(), outcome });
    }

    // The bench is armed by a boot parameter, so a boot without it owes
    // nothing; a boot with it owes every percentile on one line.
    if let Some(line) = log.lines().find(|l| l.contains(TLB_BENCH)) {
        for (about, head, bound) in TLB_PERCENTILES {
            let outcome = match number(line, head, "ns") {
                Some(value) => bound.check(value, "ns"),
                None => Outcome::Failed(format!("the bench line carries no {head:?}: {line}")),
            };
            out.push(Verdict { about: (*about).to_string(), outcome });
        }
    }

    out.push(Verdict { about: CYCLICTEST.to_string(), outcome: wake_latency(log) });
    out.push(Verdict {
        about: SCHED_STRESS.to_string(),
        outcome: match exit_of(log, SCHED_STRESS) {
            Some(exit) if exit.code == 0 => Outcome::Held(format!("cpu {}ms", exit.cpu_ms)),
            Some(exit) => Outcome::Failed(format!("the stress suite exited {}", exit.code)),
            None => Outcome::Failed(format!("the boot carries no `exit: {SCHED_STRESS}` record")),
        },
    });
    out
}

fn wake_latency(log: &str) -> Outcome {
    let Some(exit) = exit_of(log, CYCLICTEST) else {
        return Outcome::Failed(format!("the boot carries no `exit: {CYCLICTEST}` record"));
    };
    if exit.code == CYCLICTEST_FAILED {
        return Outcome::Failed(
            "cyclictest measured nothing — the real-time band was refused or never endowed".into(),
        );
    }
    CYCLICTEST_BOUND.check(exit.code, "us p99 wake lateness")
}

/// Every value a boot reported that a profile entry would hold, whether or not
/// this profile holds one yet: the lines a reader needs to write the next
/// version of this file.
pub fn inventory(log: &str) -> Vec<String> {
    let mut out = Vec::new();
    for needle in [
        "ACPI: ",
        "pmm: the firmware map",
        "clock: ",
        "TSC: ",
        "LAPIC timer: ",
        "SMP: ",
        "control_regs: ",
        "iommu: DMAR",
        "iommu: rmrr",
        "PCI: Enumeration complete",
        "tlb: bench ",
        "Boot: complete (",
        "exit: test_rs_",
    ] {
        for line in log.lines().filter(|l| l.contains(needle)) {
            let line = line.trim().to_string();
            if !out.contains(&line) {
                out.push(line);
            }
        }
    }
    out
}

/// The word between `head` and `tail` on the first line carrying **both**,
/// parsed.
///
/// Both, not just the head: a boot log has many lines that begin a field name
/// and do not carry the field, and a reader that took the first of those would
/// answer about the wrong record instead of saying it found none.
fn number(log: &str, head: &str, tail: &str) -> Option<i64> {
    log.lines().find_map(|line| {
        let (_, rest) = line.split_once(head)?;
        let (word, _) = if tail.is_empty() { (rest, "") } else { rest.split_once(tail)? };
        word.trim().parse().ok()
    })
}

/// The first line carrying `needle`, trimmed, for a verdict to quote.
fn quoted(log: &str, needle: &str) -> String {
    log.lines().find(|line| line.contains(needle)).map(|line| line.trim().to_string()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A boot of `tests/latencycase` as this tree's records spell one, with the
    /// numbers a q35 guest answered — the machine is a control and not the
    /// subject, but the *grammar* is the same one the T14 writes.
    fn a_good_boot() -> String {
        [
            "[kernel 0.010 cpu0] ACPI: RSDP at 0x7fb7e014",
            "[kernel 0.011 cpu0] ACPI: APIC at 0x7fb78000 len=128 rev=3 oem=\"BOCHS\" checksummed",
            "[kernel 0.012 cpu0] ACPI: 5 of 5 tables checksummed under the RSDP at 0x7fb7e014",
            "[kernel 0.013 cpu0] pmm: the firmware map calls 4288675840 bytes usable in 6 \
             entries; managed=4239392768 withheld=25165824 unaligned=24117248, and the three sum \
             to it; frames=2022 base=0x100000 span=2047",
            "[kernel 0.050 cpu0] clock: TSC measured 999780048Hz against the HPET, CPUID states \
             1000000000Hz, 219ppm apart",
            "[kernel 0.061 cpu0] LAPIC timer: 10010791 ticks/10ms, so 1001079100Hz",
            "[kernel 0.081 cpu0] PCI: Enumeration complete, 9 functions.",
            "[kernel 0.090 cpu0] iommu: DMAR describes 1 units and 2 reserved regions, 2 of them \
             inside the identity domain",
            "[kernel 0.301 cpu0] SMP: 8 of 8 MADT cpus online, 7 of 7 APs inside the BSP's TSC \
             bracket",
            "[kernel 0.302 cpu0] control_regs: 8 of 8 cpus hold cr0=0x80010033 cr4=0x00300668 \
             efer=0xd01",
            "[kernel 0.401 cpu0] tlb: bench 256 shootdowns across 8 cpus min=34007ns p50=60011ns \
             p90=77014ns p99=250045ns max=3043547ns",
            "[kernel 0.500 cpu0] Boot: complete (500ms)",
            "[kernel 3.100 cpu1] exit: test_rs_cyclictest pid=6 code=41 cpu=2100ms",
            "[kernel 5.900 cpu1] exit: test_rs_sched_stress pid=7 code=0 cpu=1800ms",
        ]
        .join("\n")
    }

    fn failures(log: &str) -> Vec<String> {
        judge(log).into_iter().filter(|v| v.outcome.is_failure()).map(|v| v.about).collect()
    }

    #[test]
    fn a_whole_boot_holds_every_row() {
        assert_eq!(failures(&a_good_boot()), Vec::<String>::new());
    }

    /// One negative control per relation, because a relation that cannot be
    /// broken is not being checked.
    #[test]
    fn each_relation_has_a_mutation_that_breaks_it() {
        // A byte unaccounted for.
        let short = a_good_boot().replace("unaligned=24117248", "unaligned=24117247");
        assert_eq!(failures(&short), ["pmm-balances"]);

        // An AP whose TSC read outside the bracket. The count and the word the
        // kernel spells it with both move, which is the point of having two.
        let skewed = a_good_boot().replace("7 of 7 APs", "6 of 7 APs");
        assert_eq!(failures(&skewed), ["tsc-bracket"]);
        let named = format!("{}\n[kernel 0.2 cpu0] SMP: cpu3 tsc=1 trails the BSP's 9..10 by 8 cycles", a_good_boot());
        assert_eq!(failures(&named), ["tsc-trail"]);

        // A CPU that came up and never reached `control_regs::init`.
        let unchecked = a_good_boot().replace("control_regs: 8 of 8", "control_regs: 7 of 8");
        assert_eq!(failures(&unchecked), ["control-regs-count"]);

        // A reserved region the identity domain does not cover.
        let outside = a_good_boot().replace("2 of them inside", "1 of them inside");
        assert_eq!(failures(&outside), ["rmrr-held"]);
        let spelled = format!(
            "{}\n[kernel 0.09 cpu0] iommu: rmrr0 seg=0 0x0..0x1 OUTSIDE the identity domain 0x0..0x2",
            a_good_boot()
        );
        assert_eq!(failures(&spelled), ["rmrr-outside"]);

        // The instrument that measured nothing, and the one that failed.
        let refused = a_good_boot().replace("test_rs_cyclictest pid=6 code=41", "test_rs_cyclictest pid=6 code=255");
        assert_eq!(failures(&refused), [CYCLICTEST]);
        let stressed = a_good_boot().replace("test_rs_sched_stress pid=7 code=0", "test_rs_sched_stress pid=7 code=1");
        assert_eq!(failures(&stressed), [SCHED_STRESS]);
    }

    /// A bound reds when the machine leaves it, in both directions.
    #[test]
    fn a_bound_is_judged_once_it_is_taken() {
        let unit = "us p99 wake lateness";
        assert!(matches!(Bound::AtMost(50).check(41, unit), Outcome::Held(_)));
        assert!(Bound::AtMost(40).check(41, unit).is_failure());
        assert!(matches!(Bound::Exactly(8).check(8, "cpus"), Outcome::Held(_)));
        assert!(Bound::Exactly(8).check(7, "cpus").is_failure());
        assert!(matches!(Bound::Unmeasured.check(41, unit), Outcome::Measured { value: 41, .. }));
    }

    /// The needles are prefixes of records this tree's kernel writes, held to
    /// the kernel's own source because nothing links the two crates.
    #[test]
    fn the_kernel_writes_the_records_this_reads() {
        let wanted: &[(&str, &str)] = &[
            ("kernel/src/drivers/acpi.rs", "ACPI: RSDP at "),
            ("kernel/src/drivers/acpi.rs", " tables checksummed under the RSDP"),
            ("kernel/src/mm/pmm.rs", "pmm: the firmware map calls "),
            ("kernel/src/clock.rs", "clock: TSC measured "),
            ("kernel/src/arch/apic.rs", "LAPIC timer: "),
            ("kernel/src/drivers/pci.rs", "PCI: Enumeration complete, "),
            ("kernel/src/arch/smp.rs", " MADT cpus online, "),
            ("kernel/src/arch/smp.rs", "trails the BSP's"),
            ("kernel/src/arch/smp.rs", "leads the BSP's"),
            ("kernel/src/arch/control_regs.rs", "control_regs: "),
            ("kernel/src/iommu/vtd/mod.rs", "iommu: DMAR describes "),
            ("kernel/src/iommu/vtd/mod.rs", "OUTSIDE the identity domain"),
            ("kernel/src/arch/tlb.rs", TLB_BENCH),
            ("kernel/src/process.rs", "exit: {name} pid="),
        ];
        for (file, needle) in wanted {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(file);
            let source = std::fs::read_to_string(&path).expect("a kernel module");
            // Whitespace-folded and with the continuation dropped, because
            // rustfmt wraps a long format string across lines with a lone `\`
            // that is not part of the string the kernel writes.
            let folded: String =
                source.split_whitespace().filter(|w| *w != "\\").collect::<Vec<_>>().join(" ");
            let want: String = needle.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(folded.contains(&want), "{} writes no {needle:?}", path.display());
        }
    }
}
