//! Stage I1: what the kernel read off the machine's remapping units.
//!
//! The trap this gate exists to avoid is the one a discovery test falls into
//! by default. A kernel that printed a plausible capability line without
//! reading a register would satisfy any single-machine assertion, and so would
//! a decode reading the wrong bits of the right register. So the assertions
//! are not "the line is there": three machines are booted whose units differ
//! in exactly one advertised capability each, and the gate is that the guest's
//! decode *moves with them*. A constant cannot track a register it never read.
//!
//! Ground truth is split, deliberately. Whether the unit exists at all is
//! invisible to every console line — a kernel that says "no DMAR" on a machine
//! that has one and a harness that forgot the device produce the same log — so
//! presence is checked against the argv, which is the host side of the device.
//! What the unit *says* can only come from the guest, so that half is checked
//! against the console.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

use super::qemu::{self, BootOptions, Profile, QemuInstance};

/// Offsets into a unit's register window, Sections 11.4.4.2, 11.4.6 and 11.4.10.
const GSTS_REG: u64 = 0x1C;
const RTADDR_REG: u64 = 0x20;
const IRTA_REG: u64 = 0xB8;

/// Bits 51:12 of a root, context or second-level entry, Sections 9.1 to 9.8.
const ENTRY_ADDR: u64 = 0x000F_FFFF_FFFF_F000;
/// The one leaf size this kernel writes.
const PAGE_2M: u64 = 2 * 1024 * 1024;
use super::serial::Serial;

/// The five machines, and what each one moves.
///
/// [`Profile::Metal`] is the reference: the configuration every other profile
/// in the suite runs, so a difference below is a difference the profile made
/// and not one the shape did — all five are metal-sim and differ in the unit
/// alone.
const MACHINES: &[Profile] = &[
    Profile::Metal,
    Profile::NoIommu,
    Profile::IommuNarrow,
    Profile::IommuNoIntremap,
    Profile::IommuEim,
];

pub fn iommu_discovery(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let mut decoded: BTreeMap<&str, BTreeMap<String, String>> = BTreeMap::new();

    for &profile in MACHINES {
        let name = profile_name(profile);
        let options = BootOptions { profile, qmp: true, ..Default::default() };
        argv_check(profile, &qemu::profile_argv(&options))?;

        let qemu = QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);
        let log = Serial::boot(&qemu);
        // Discovery runs in the storage phase, long before userland; a machine
        // that did not finish booting is a machine whose log says nothing
        // about what came after the unit.
        log.must_be_clean()?;
        log.must_say("Boot: complete")?;

        let Some(unit) = profile.iommu() else {
            interrupt_format(&log, name, None, None)?;
            // `Absent` is firmware answering the question, and the answer is
            // one a user can act on — so the line names the firmware setting
            // as well as the hardware. What makes this assertion mean
            // something is the pair below it: a kernel that always printed
            // this would fail on every other machine here.
            log.must_say("iommu: no DMAR table")?;
            log.must_say("VT-d is disabled in firmware setup")?;
            log.must_not_say("iommu: unit")?;
            log.must_not_say("iommu: DMAR haw=")?;
            eprintln!("  [iommu] {name}: no DMAR, and no unit described");
            continue;
        };

        // The kernel reports the width one greater than the field holds, so a
        // machine declaring 48 bits of host address is the aw-bits the profile
        // asked for. Both halves of the table's own header are asserted:
        // `INTR_REMAP` is a platform-level flag and `ECAP.IR` below is the
        // unit's, and the kernel refuses on them separately.
        log.must_say(&format!("iommu: DMAR haw={}", unit.aw_bits))?;
        log.must_say(&format!(
            "intr_remap={}",
            if unit.intremap { 'y' } else { 'n' }
        ))?;

        let line = log.must_say("iommu: unit0 @")?;
        let fields = unit_fields(line);
        let field = |k: &str| -> Result<String, String> {
            fields
                .get(k)
                .cloned()
                .ok_or_else(|| format!("{name}: the unit line has no {k}= field: {line:?}"))
        };

        // The decode, against what the profile asked QEMU for.
        expect(&field("aw")?, &unit.aw_bits.to_string(), "aw", name, line)?;
        expect(&field("ir")?, if unit.intremap { "y" } else { "n" }, "ir", name, line)?;
        expect(&field("eim")?, if unit.eim { "y" } else { "n" }, "eim", name, line)?;
        // Not a profile dimension, and asserted because the whole suite rests
        // on it: `caching-mode=on` is what makes QEMU's IOTLB a real cache and
        // the map-side invalidation load-bearing, and 2 MiB
        // leaf entries are what this kernel's one page size requires.
        expect(&field("cm")?, "y", "cm", name, line)?;
        expect(&field("sps2m")?, "y", "sps2m", name, line)?;

        // Stage I2, on the guest's own word. It is the weakest of the three
        // things that certify it and it is here because it costs no boot: the
        // suite booting green with the unit on is the second, and the two
        // actuator gates below — a device the unit blocks — are the only ones
        // that can tell translation from a unit that is merely switched on.
        let unit_line = log.must_say("translating gsts=")?;
        let tes = unit_fields(unit_line)
            .get("tes")
            .cloned()
            .ok_or_else(|| format!("{name}: no tes= on {unit_line:?}"))?;
        expect(&tes, "y", "tes", name, unit_line)?;

        // Every scope naming a PCI function must name one this machine has.
        // A decode that read the path bytes at the wrong offset would produce
        // requester ids that look like addresses and match no device.
        let scopes = scope_check(&log, name)?;

        // The `intremap=off` machine is what makes this mean something: the same
        // parser over the same lines has to reach the opposite verdict on it.
        interrupt_format(&log, name, Some(qemu.qmp_socket()), unit.intremap.then_some(unit.eim))?;

        eprintln!(
            "  [iommu] {name}: aw={} ir={} cap={} ecap={} — {scopes} PCI scopes matched",
            field("aw")?,
            field("ir")?,
            field("cap")?,
            field("ecap")?
        );
        decoded.insert(name, fields);
    }

    // The negative control, and the reason this test boots five machines
    // instead of one. Each pair below differs in one QEMU knob, so a decode
    // that reports the same value for both is a decode that is not reading the
    // register the knob moves.
    for (a, b, key) in [
        (profile_name(Profile::Metal), profile_name(Profile::IommuNarrow), "aw"),
        (profile_name(Profile::Metal), profile_name(Profile::IommuNoIntremap), "ir"),
        (profile_name(Profile::Metal), profile_name(Profile::IommuEim), "eim"),
    ] {
        let (Some(left), Some(right)) = (decoded.get(a), decoded.get(b)) else {
            return Err(format!("{a} or {b} produced no unit line to compare"));
        };
        let (Some(lv), Some(rv)) = (left.get(key), right.get(key)) else {
            return Err(format!("no {key}= on {a} or {b}"));
        };
        if lv == rv {
            return Err(format!(
                "{a} and {b} both report {key}={lv}, but their units advertise different \
                 capabilities — the kernel is printing a constant, not decoding a register"
            ));
        }
        // And the raw register the field came out of has to have moved too. A
        // decode of the right register reported through the wrong field would
        // pass the line above on one of these pairs by accident.
        let raw = if key == "aw" { "cap" } else { "ecap" };
        let (Some(lr), Some(rr)) = (left.get(raw), right.get(raw)) else {
            return Err(format!("no {raw}= on {a} or {b}"));
        };
        if lr == rr {
            return Err(format!(
                "{a} and {b} report {key}={lv}/{rv} out of the same {raw}={lr} — the value did \
                 not come from that register"
            ));
        }
        eprintln!("  [iommu] {a} vs {b}: {key} {lv} != {rv}, out of {raw} {lr} != {rr}");
    }

    destination_encoding(test_config, c_bins, rust_bins)
}

/// The two ways an entry can name a CPU, told apart.
///
/// `EIME` puts a 32-bit id at `DST` 63:32 and its absence an 8-bit one at 47:40
/// (Section 9.9) — the same bits for APIC 0, which is where every interrupt in
/// this kernel goes, so a kernel with the two backwards boots green everywhere.
/// `iommu-dest-apic1` moves the device messages to APIC 1, where they differ.
fn destination_encoding(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let mut seen = Vec::new();
    for (profile, extended) in [(Profile::Metal, false), (Profile::IommuEim, true)] {
        let options = BootOptions {
            profile,
            qmp: true,
            kernel_params: &["iommu-dest-apic1"],
            ..Default::default()
        };
        let qemu = QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);
        let log = Serial::boot(&qemu);
        log.must_be_clean()?;
        log.must_say("Boot: complete")?;
        let name = profile_name(profile);
        interrupt_format(&log, name, Some(qemu.qmp_socket()), Some(extended))?;

        // A PCI function's entry: the pins keep the boot CPU either way.
        let entries = table_entries(&log, name)?;
        let moved = entries
            .iter()
            .find(|e| e.apic == 1)
            .ok_or_else(|| format!("{name}: no entry was moved to APIC 1 by the actuator"))?;
        let base = interrupt_table_base(&log, name, qemu.qmp_socket())?;
        let (lo, _) = table_word(qemu.qmp_socket(), base, moved.index)?;
        seen.push((name, lo >> 32));
        eprintln!("  [iommu] {name}: APIC 1 encodes as DST {:#x}", lo >> 32);
    }
    let [(a, left), (b, right)] = seen[..] else {
        return Err("the destination arm booted the wrong number of machines".to_string())
    };
    if left == right {
        return Err(format!(
            "{a} and {b} both put APIC 1 at DST {left:#x}, and one has EIME set and the other \
             does not — the encoding is not moving with the mode"
        ));
    }
    Ok(())
}

/// The table's address out of `IRTA_REG`, over the monitor.
fn interrupt_table_base(log: &Serial, name: &str, socket: &Path) -> Result<u64, String> {
    let window = register_window(socket, log, name)?;
    Ok(over_qmp(socket, window + IRTA_REG, 1, 'g')?[0] & !0xFFF)
}

fn table_word(socket: &Path, base: u64, index: u16) -> Result<(u64, u64), String> {
    let words = over_qmp(socket, base + u64::from(index) * 16, 2, 'g')?;
    Ok((words[0], words[1]))
}

/// Every interrupt source in the machine, in the format the hardware holds it.
///
/// Stage I3, and the trap is a source nobody moved. The specification blocks a
/// compatibility-format message under `IRE` with `CFI` clear, so on real
/// hardware a source left behind is a device that has silently stopped — but
/// QEMU delivers it anyway
/// (`issues/kernel/qemu-passes-compatibility-format-interrupts.md`), so no
/// behavioural test in this suite can see one and this is the only thing that
/// can. So it starts at hardware: `GSTS` and `IRTA_REG` are read out of the
/// unit's register window over the monitor, the table is read at **the address
/// `IRTA_REG` holds** and not the one the kernel printed, and every requester id
/// is checked against this machine's PCI walk and DMAR scope. The kernel's line
/// is checked against all of it — naming a page the register does not hold reds.
///
/// [`Profile::Headless`] carries the most sources of both kinds — the i8042's
/// two pins, and xHCI, virtio-net and virtio-sound over MSI-X.
///
/// **Per pull request this is the one machine that runs.** The machines that
/// make the verdict move — `intremap=off`, and `eim=on` for the other entry
/// format — are [`iommu_discovery`]'s, and that is nightly, so a change that
/// broke only the not-remapping arm would land and be caught the next night.
pub fn iommu_interrupt_remapping(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let options = BootOptions { profile: Profile::Headless, qmp: true, ..Default::default() };
    unit_is_first(&qemu::profile_argv(&options), "headless")?;
    let qemu = QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);
    let log = Serial::boot(&qemu);
    log.must_be_clean()?;
    log.must_say("Boot: complete")?;
    interrupt_format(&log, "headless", Some(qemu.qmp_socket()), Some(false))
}

/// `count` words of guest *physical* address space at `base`, over the monitor.
/// `xp` reaches the unit's MMIO window as readily as RAM, which is what lets the
/// checks below start at a register rather than at a number the guest printed.
fn over_qmp(socket: &Path, base: u64, count: usize, width: char) -> Result<Vec<u64>, String> {
    let dump = qemu::QmpMonitor::open(socket).human(&format!("xp/{count}x{width} 0x{base:x}"));
    let mut words = Vec::new();
    for token in dump.split_whitespace() {
        let Some(hex) = token.strip_prefix("0x") else { continue };
        let hex = hex.trim_end_matches(|c: char| !c.is_ascii_hexdigit());
        words.push(
            u64::from_str_radix(hex, 16)
                .map_err(|_| format!("unreadable word {token:?} in\n{dump}"))?,
        );
    }
    if words.len() != count {
        return Err(format!(
            "the monitor returned {} words for {count} at {base:#x}:\n{dump}",
            words.len()
        ));
    }
    Ok(words)
}

/// One machine's sources, judged against whether its unit remaps at all and,
/// if it does, whether its entries name a 32-bit destination.
fn interrupt_format(
    log: &Serial,
    name: &str,
    socket: Option<&Path>,
    mode: Option<bool>,
) -> Result<(), String> {
    let remapping = mode.is_some();
    let entries = table_entries(log, name)?;
    if remapping != !entries.is_empty() {
        return Err(format!(
            "{name}: the unit remaps interrupts = {remapping}, and the kernel wrote {} table \
             entries. Neither number is allowed to move without the other",
            entries.len()
        ));
    }

    // A machine with no unit prints no unit line, and must be one that does not remap.
    let sources = source_formats(log, name)?;
    if sources.is_empty() {
        return Err(format!(
            "{name}: this machine armed no interrupt source at all, so there is nothing here to \
             be in the right format"
        ));
    }
    for source in &sources {
        if source.remappable != remapping {
            return Err(format!(
                "{name}: {} is in {} format and the unit remaps = {remapping}. Under IRE with \
                 CFI clear a compatibility-format message is blocked, so this source has stopped",
                source.who,
                if source.remappable { "remappable" } else { "compatibility" }
            ));
        }
    }

    let Some(line) = log.text().lines().find(|l| l.contains("translating gsts=")).map(str::to_string)
    else {
        if remapping {
            return Err(format!("{name}: no unit is translating, so none can be remapping"));
        }
        return report(log, name, mode, &sources);
    };
    let fields = unit_fields(&line);
    let field = |k: &str| -> Result<String, String> {
        fields.get(k).cloned().ok_or_else(|| format!("{name}: no {k}= on {line:?}"))
    };
    let socket = socket.ok_or_else(|| format!("{name}: this gate needs BootOptions {{ qmp }}"))?;
    let window = register_window(socket, log, name)?;

    // GSTS and IRTA_REG out of that window, and the kernel's line checked
    // against them — the only direction that catches a kernel misreporting them.
    let gsts = over_qmp(socket, window + GSTS_REG, 1, 'w')?[0] as u32;
    let irta = over_qmp(socket, window + IRTA_REG, 1, 'g')?[0];
    let ires = gsts & (1 << 25) != 0;
    let cfis = gsts & (1 << 23) != 0;
    expect(&field("gsts")?, &format!("{gsts:#010x}"), "gsts", name, &line)?;
    expect(&field("ires")?, if ires { "y" } else { "n" }, "ires", name, &line)?;
    expect(&field("cfis")?, if cfis { "y" } else { "n" }, "cfis", name, &line)?;
    expect(&field("irta")?, &format!("{irta:#x}"), "irta", name, &line)?;
    if ires != remapping {
        return Err(format!("{name}: GSTS.IRES is {ires} where the unit remaps = {remapping}\n{line}"));
    }
    // `CFI` is the bit that would let a compatibility message through. It cannot
    // fail here — QEMU defines VTD_GCMD_CFI and VTD_GSTS_CFIS and references
    // neither — so it states the kernel's intent for whoever reads it on
    // hardware; it is not an instrument.
    if cfis {
        return Err(format!(
            "{name}: GSTS.CFIS is set, so the unit passes compatibility-format interrupts through \
             unremapped\n{line}"
        ));
    }

    let mut memory = Vec::new();
    if let Some(extended) = mode {
        // The address the unit walks is IRTA's, never the kernel's `irt=`.
        let base = irta & !0xFFF;
        if (irta & (1 << 11) != 0) != extended {
            return Err(format!(
                "{name}: IRTA_REG is {irta:#x}, whose EIME is {} where this unit's ECAP.EIM says \
                 {extended}\n{line}",
                irta & (1 << 11) != 0
            ));
        }
        expect(&field("irt")?, &format!("{base:#x}"), "irt", name, &line)?;
        let highest = entries.iter().map(|e| e.index).max().unwrap_or(0) as usize;
        memory = over_qmp(socket, base, (highest + 1) * 2, 'g')?
            .chunks(2)
            .map(|pair| (pair[0], pair[1]))
            .collect();
    }

    if !remapping {
        return report(log, name, mode, &sources);
    }

    // Two requester ids that no single source could produce: a PCI function's,
    // which the walk printed, and the I/O APIC's, which sits on a pseudo-bus no
    // walk reaches and exists only in the DMAR scope.
    let functions = enumerated_functions(log);
    let apics = scope_sources(log);
    if apics.is_empty() {
        return Err(format!("{name}: the unit named no I/O APIC scope to take a source id from"));
    }

    // The handle a source carries has to reach the entry that verifies *its
    // own* requester id. A source pointed at somebody else's entry would be
    // refused by the unit for source-id verification, and a gate that only
    // asked whether the entry existed would call that correct.
    for source in &sources {
        let want = match &source.requester {
            Requester::Function(bdf) => bdf.clone(),
            Requester::Controller(id) => apics.get(id).cloned().ok_or_else(|| {
                format!("{name}: {} sits on a chip the unit's scopes never named", source.who)
            })?,
        };
        let Some(entry) = entries.iter().find(|e| e.index == source.handle) else {
            return Err(format!(
                "{name}: {} carries handle {}, and the kernel wrote no table entry with that \
                 index — the unit would refuse it as out of bounds",
                source.who, source.handle
            ));
        };
        if entry.source != want {
            return Err(format!(
                "{name}: {} carries handle {}, and irte{} is verified against {} rather than \
                 {want} — the unit refuses that message for source-id verification",
                source.who, source.handle, entry.index, entry.source
            ));
        }
    }

    let mut from_pci = 0usize;
    let mut from_apic = 0usize;
    for entry in &entries {
        let (lo, hi) = memory[entry.index as usize];
        // Section 9.9: P bit 0, V 23:16, DST 63:32, SID 79:64, SQ 81:80, SVT 83:82.
        let (svt, sq, sid) = ((hi >> 18) & 0x3, (hi >> 16) & 0x3, hi & 0xFFFF);
        if svt != 1 || sq != 0 {
            return Err(format!(
                "{name}: irte{} is SVT={svt} SQ={sq} in the memory the unit reads, so a message \
                 carrying any other requester id would be remapped through it. Every entry is \
                 verified against all sixteen bits of one source id",
                entry.index
            ));
        }
        if lo & 1 == 0 {
            return Err(format!("{name}: irte{} is not Present in memory", entry.index));
        }
        // Not two witnesses: the kernel's read of these bytes against the
        // host's, which catches it reporting a table the register does not name.
        if format!("{sid:#06x}") != entry.sid {
            return Err(format!(
                "{name}: irte{} carries SID {sid:#06x} in memory and the kernel reported {}",
                entry.index, entry.sid
            ));
        }
        // `entry.apic` is the id the kernel was *given*; `DST` is where it put
        // it. Section 9.9 puts a 32-bit id at 63:32 under EIME and an 8-bit one
        // at 47:40 without, so the two differ for every id but 0.
        let dst = lo >> 32;
        let want = if mode == Some(true) { entry.apic } else { entry.apic << 8 };
        if dst != want {
            return Err(format!(
                "{name}: irte{} has DST {dst:#x} where APIC {:#x} in {} mode encodes as {want:#x}",
                entry.index,
                entry.apic,
                if mode == Some(true) { "extended" } else { "xAPIC" }
            ));
        }
        if apics.values().any(|sid| *sid == entry.source) {
            from_apic += 1;
        } else if functions.contains(&entry.source) {
            from_pci += 1;
        } else {
            return Err(format!(
                "{name}: irte{} is verified against {}, which is neither a function this machine \
                 enumerated ({functions:?}) nor an I/O APIC the unit scoped ({apics:?})",
                entry.index, entry.source
            ));
        }
    }
    if from_pci == 0 || from_apic == 0 {
        return Err(format!(
            "{name}: {from_pci} entries name a PCI function and {from_apic} name the I/O APIC. \
             Both paths into the unit have to be covered or half of this gate is vacuous"
        ));
    }
    let indices: BTreeSet<u16> = entries.iter().map(|e| e.index).collect();
    if indices.len() != entries.len() {
        return Err(format!(
            "{name}: {} entries over {} distinct indices — two sources share a handle, so one \
             of them is delivered as the other",
            entries.len(),
            indices.len()
        ));
    }

    report(log, name, mode, &sources)?;
    eprintln!(
        "  [iommu] {name}: {} entries at the address IRTA_REG holds ({from_pci} pci, \
         {from_apic} ioapic), all SVT=1 SQ=0 Present",
        entries.len()
    );
    Ok(())
}

fn report(log: &Serial, name: &str, mode: Option<bool>, sources: &[Source]) -> Result<(), String> {
    let _ = log;
    match mode {
        None => eprintln!(
            "  [iommu] {name}: no remapping, and all {} source(s) in compatibility format",
            sources.len()
        ),
        Some(extended) => eprintln!(
            "  [iommu] {name}: IRES=1 CFIS=0 EIME={}, {} source(s) remappable",
            u8::from(extended),
            sources.len()
        ),
    }
    Ok(())
}

/// What the kernel reported reading back out of one entry, all of it cross-checked against memory.
struct Entry {
    index: u16,
    source: String,
    sid: String,
    /// The APIC id it was handed, as against the `DST` field it encoded into.
    apic: u64,
}

fn table_entries(log: &Serial, name: &str) -> Result<Vec<Entry>, String> {
    let mut entries = Vec::new();
    for line in log.text().lines() {
        let Some(rest) = line.split("iommu: irte").nth(1) else { continue };
        let (index, _) = rest
            .split_once(' ')
            .ok_or_else(|| format!("{name}: unreadable table entry line: {line:?}"))?;
        let index: u16 = index
            .parse()
            .map_err(|_| format!("{name}: {index:?} is not an entry index: {line:?}"))?;
        let fields = unit_fields(line);
        let field = |k: &str| -> Result<String, String> {
            fields.get(k).cloned().ok_or_else(|| format!("{name}: no {k}= on {line:?}"))
        };
        entries.push(Entry {
            index,
            source: field("source")?,
            sid: field("sid")?,
            apic: u64::from_str_radix(field("apic")?.trim_start_matches("0x"), 16)
                .map_err(|_| format!("{name}: unreadable apic on {line:?}"))?,
        });
    }
    Ok(entries)
}

enum Requester {
    /// A PCI function, which the walk printed as `bb:dd.f`.
    Function(String),
    /// An interrupt controller, by MADT id: its requester id exists only in the DMAR.
    Controller(String),
}

struct Source {
    who: String,
    requester: Requester,
    remappable: bool,
    handle: u16,
}

fn source_formats(log: &Serial, name: &str) -> Result<Vec<Source>, String> {
    let mut sources = Vec::new();
    for line in log.text().lines() {
        let fields = unit_fields(line);
        if let Some(rest) = line.split("ioapic: gsi ").nth(1) {
            let gsi = rest.split(' ').next().unwrap_or_default();
            let (Some(id), Some(rte)) = (fields.get("id"), fields.get("rte")) else {
                return Err(format!("{name}: unreadable redirection entry line: {line:?}"));
            };
            let rte = u64::from_str_radix(rte.trim_start_matches("0x"), 16)
                .map_err(|_| format!("{name}: unreadable rte on {line:?}"))?;
            // Figure 5-3: format bit 48, index 63:49, index[15] at bit 11.
            sources.push(Source {
                who: format!("the pin on GSI {gsi}"),
                requester: Requester::Controller(id.clone()),
                remappable: rte & (1 << 48) != 0,
                handle: ((rte >> 49) & 0x7FFF) as u16 | (((rte >> 11) & 1) as u16) << 15,
            });
        } else if line.contains(": msix address=") || line.contains(": msi address=") {
            let (Some(address), Some(data)) = (fields.get("address"), fields.get("data")) else {
                return Err(format!("{name}: unreadable message line: {line:?}"));
            };
            let Some(who) = line
                .split("PCI ")
                .nth(1)
                .and_then(|r| r.split_whitespace().next())
                .map(|bdf| bdf.trim_end_matches(':'))
            else {
                return Err(format!("{name}: a message line naming no function: {line:?}"));
            };
            let address = u32::from_str_radix(address.trim_start_matches("0x"), 16)
                .map_err(|_| format!("{name}: unreadable message address on {line:?}"))?;
            let data = u32::from_str_radix(data.trim_start_matches("0x"), 16)
                .map_err(|_| format!("{name}: unreadable message data on {line:?}"))?;
            // Figure 5-4: format bit 4, SHV bit 3, handle 19:5, handle[15] at bit 2.
            let remappable = address & (1 << 4) != 0;
            let handle = ((address >> 5) & 0x7FFF) as u16 | (((address >> 2) & 1) as u16) << 15;
            if remappable && (address & (1 << 3) == 0 || data != 0) {
                return Err(format!(
                    "{name}: {who} writes a remappable message with SHV={} and data={data:#x}; \
                     Figure 5-4 sets SHV and programs the data register to 0h, and the index the \
                     unit computes is handle plus subhandle",
                    (address >> 3) & 1
                ));
            }
            sources.push(Source {
                who: format!("the message-signalled interrupt of {who}"),
                requester: Requester::Function(who.to_string()),
                remappable,
                handle,
            });
        }
    }
    Ok(sources)
}

/// The requester id the unit's scopes give each interrupt controller, by MADT id.
fn scope_sources(log: &Serial) -> BTreeMap<String, String> {
    let mut named = BTreeMap::new();
    for line in log.text().lines() {
        let Some(rest) = line.split("scope ioapic ").nth(1) else { continue };
        let Some(sid) = rest.split(' ').next() else { continue };
        if let Some(id) = unit_fields(line).get("id") {
            named.insert(id.clone(), sid.to_string());
        }
    }
    named
}

/// Whether this machine's virtio functions are behind the unit at all.
///
/// QEMU keeps a virtio function on `&address_space_memory` — the unit bypassed,
/// whatever the tables say — unless it is created with `iommu_platform=on`
/// (`hw/virtio/virtio-bus.c:86-99`, `hw/virtio/virtio-pci.c:1400-1405` at
/// v11.1.0), and under identity mapping the two are indistinguishable. So the
/// argv says which functions were created behind a unit and the console says
/// which negotiated `VIRTIO_F_ACCESS_PLATFORM`. [`Profile::HeadlessNoIommu`] is
/// the same machine without one, and there the guest reports `n` because QEMU
/// never *offers* the bit (`hw/virtio/virtio-bus.c:87-94`) rather than because
/// the driver declined — this driver offers blindly. What makes the pair mean
/// something is [`declining_is_not_free`], where the guest really does decline.
///
/// virtio-sound is a declared exception, asserted rather than tolerated: its
/// function must be the one that did *not* negotiate
/// (`issues/kernel/three-devices-still-reach-all-of-memory.md`).
pub fn iommu_virtio_platform(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    for profile in [Profile::Headless, Profile::HeadlessNoIommu] {
        let name = if profile.iommu().is_some() { "headless" } else { "headless-no-iommu" };
        let behind_unit = profile.iommu().is_some();
        let options = BootOptions { profile, ..Default::default() };

        let argv = qemu::profile_argv(&options);
        let created: Vec<&str> = argv
            .windows(2)
            .filter(|w| w[0] == "-device")
            .map(|w| w[1].as_str())
            .filter(|d| d.starts_with("virtio-") && d.contains("-pci"))
            .collect();
        if created.is_empty() {
            return Err(format!("{name}: this machine creates no virtio function at all"));
        }
        // Ahead of everything it decodes, or a function created before it keeps
        // the bypassing address space and `iommu_platform=on` changes nothing
        // (`hw/virtio/virtio-bus.c:97`).
        unit_is_first(&argv, name)?;
        for device in &created {
            let want = behind_unit && !device.starts_with(SOUND);
            if device.contains("iommu_platform=on") != want {
                return Err(format!(
                    "{name}: the machine has a unit = {behind_unit} and QEMU is given {device}, \
                     where iommu_platform=on is owed = {want}. A virtio function without it keeps \
                     the address space the unit does not decode, and virtio-sound is the one \
                     device ruled to keep it that way"
                ));
            }
        }

        let qemu = QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);
        let log = Serial::boot(&qemu);
        log.must_be_clean()?;
        log.must_say("Boot: complete")?;

        let mut negotiated = Vec::new();
        for line in log.text().lines() {
            let Some(rest) = line.split("VirtIO: PCI ").nth(1) else { continue };
            let Some((who, _)) = rest.split_once(' ') else {
                return Err(format!("{name}: unreadable feature line: {line:?}"));
            };
            let fields = unit_fields(line);
            let Some(accepted) = fields.get("access_platform") else { continue };
            negotiated.push((who.to_string(), accepted == "y"));
        }
        if negotiated.len() != created.len() {
            return Err(format!(
                "{name}: QEMU created {} virtio function(s) and the guest negotiated features \
                 with {} — {negotiated:?} against {created:?}",
                created.len(),
                negotiated.len()
            ));
        }
        let sound = class_function(&log, "0401");
        let enumerated = enumerated_functions(&log);
        for (who, accepted) in &negotiated {
            let want = behind_unit && Some(who.clone()) != sound;
            if *accepted != want {
                return Err(format!(
                    "{name}: {who} negotiated VIRTIO_F_ACCESS_PLATFORM = {accepted} where {want} \
                     is owed — the unit exists = {behind_unit}, and the audio function \
                     {sound:?} is the one ruled to stay outside it \
                     (issues/kernel/three-devices-still-reach-all-of-memory.md)"
                ));
            }
            if !enumerated.contains(who) {
                return Err(format!(
                    "{name}: {who} negotiated features and the PCI walk enumerated \
                     {enumerated:?} — the driver is naming a function this machine does not have"
                ));
            }
        }
        if behind_unit && !negotiated.iter().any(|(who, ok)| Some(who.clone()) == sound && !ok) {
            return Err(format!(
                "{name}: no virtio function declined VIRTIO_F_ACCESS_PLATFORM, so the audio \
                 exception this gate allows is not being exercised: {negotiated:?}"
            ));
        }
        eprintln!(
            "  [iommu] {name}: {} virtio function(s) behind a unit = {behind_unit}, audio \
             function {sound:?} outside it by ruling",
            negotiated.len()
        );
    }
    declining_is_not_free(test_config, c_bins, rust_bins)
}

/// The control that makes the two arms above mean something: a guest that
/// declines the feature its host offered gets no device, not a bypassing one.
/// `virtio_validate_features` returns `-EFAULT` and `virtio_set_status` returns
/// before it stores the status (`hw/virtio/virtio.c:2270-2276` and `:2292-2299`
/// at v11.1.0), so `FEATURES_OK` never sticks. The actuator withholds the bit
/// from every virtio device but the console; virtio-sound is never offered it,
/// so the NIC is the one device on this machine that can decline.
fn declining_is_not_free(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let qemu = QemuInstance::boot_with_options(
        test_config,
        c_bins,
        rust_bins,
        BootOptions {
            profile: Profile::Headless,
            kernel_params: &["virtio-no-access-platform"],
            ..Default::default()
        },
    );
    let log = Serial::boot(&qemu);
    log.must_be_clean()?;
    log.must_say("Boot: complete")?;

    let refused: Vec<&str> = log
        .text()
        .lines()
        .filter(|l| l.contains("refused the feature set"))
        .collect();
    if refused.is_empty() {
        return Err(format!(
            "the actuator withheld VIRTIO_F_ACCESS_PLATFORM from every virtio device but the \
             console and none was refused for it. A device the host offered it on and the guest \
             declined has to lose FEATURES_OK, and this machine went on as though the \
             negotiation were free\n{}",
            log.text()
        ));
    }
    // The console kept the bit, so this is not simply a machine with no virtio.
    log.must_say("access_platform=y")?;
    eprintln!("  [iommu] declined: {}", refused.join("\n  [iommu] declined: "));
    Ok(())
}

/// The needle that says the unit blocked something, and the marker both gates
/// below boot to. A boot that never produces it times out, which is what a
/// unit that is not translating looks like from here.
const FAULT: &str = "iommu: DMA FAULT";

/// The fault reasons a *second-level page table walk* decides, as against the
/// ones the root/context walk above it decides.
///
/// The set rather than one member, because which of them a unit gives for an
/// all-zero entry is an implementation's choice: QEMU 11.0.2 answers
/// `read-permission` — its own line reads `detected sspte permission error
/// (iova=0x1000000, level=0x4, sspte=0x0, write=0)`, so it reached the entry
/// and judged it on its permission bits rather than on a separate present bit.
/// A unit that answered `paging-entry-invalid` instead would be saying the
/// same thing. What the set excludes is the whole of the root and context
/// walk, which is the discrimination the gate needs: those are what a
/// stranded *context* entry produces, and passthrough produces no fault at all.
const SECOND_LEVEL: &[&str] = &["read-permission", "write-permission", "paging-entry-invalid"];

/// A function whose context entry the kernel deliberately never wrote must
/// fault on its first transaction, and the fault must name it.
///
/// This is the exit criterion for I2 and the isolation negative control at the
/// same time, because at this stage they are
/// the same question. Identity mapping means a translated machine and an untranslated
/// one produce the same result for every device that is *in* the tables, so
/// the only way to tell the two apart is a device that is not: with the unit
/// bypassing, or never enabled, or pointed at a context entry naming
/// passthrough, the controller below would go on working and this test would
/// wait for a fault that never comes.
///
/// [`Profile::Metal`] because it has an NVMe controller and no virtio device.
/// The distinction matters: QEMU gives a virtio device the bypassing address
/// space unless it is created with `iommu_platform=on`, so a virtio-only
/// machine could not tell a translating unit from an absent one however the
/// tables were written.
pub fn iommu_context_absent(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let (log, blocked) = fault_boot(test_config, c_bins, rust_bins, &["iommu-context-absent"])?;

    // Which function the actuator left out is decided in the guest by class
    // code; which function that *is* on this machine is read here from the PCI
    // walk's own lines. Neither half is told the other's answer, so a fault
    // naming some other device — or the actuator skipping a device the walk
    // never saw — is a failure rather than a tautology.
    let nvme = class_function(&log, "0108").ok_or_else(|| {
        format!("this machine enumerated no NVMe controller to leave out\n{}", log.text())
    })?;
    if blocked.stream != nvme {
        return Err(format!(
            "the unit blocked {} but the controller left out of the root table is {nvme}",
            blocked.stream
        ));
    }
    if blocked.reason != "context-entry-not-present" {
        return Err(format!(
            "the unit blocked {nvme} for {:?}, and a function with no context entry should be \
             blocked for having none",
            blocked.reason
        ));
    }
    eprintln!(
        "  [iommu] {nvme} left out of the root table: blocked at {} on a {} for {}",
        blocked.address, blocked.access, blocked.reason
    );
    Ok(())
}

/// A function whose context entry names a domain with nothing in it must fault
/// on its first transaction, and the fault must name the *page table* rather
/// than the entry above it.
///
/// The half [`iommu_context_absent`] cannot give. A context entry naming
/// **passthrough** would fault identically for a function that has no entry at
/// all — and would then ignore every second-level table this kernel writes,
/// which is the whole of what I4 will build on. Here the entry is present and
/// the domain behind it is empty, so a fault can only come from the unit
/// having walked a table this kernel wrote and found nothing.
///
/// It fails on a *read* deliberately. QEMU caches a translation with the
/// permissions of whichever access populated it and then lets its memory core
/// drop a later access the cached entry does not allow — silently, with no
/// fault record — so a control built on narrowing a permission hangs the boot
/// instead of faulting. That is measured, not assumed; the first thing a
/// device does here is fetch a descriptor, which
/// misses the cache and is answered by the tables.
pub fn iommu_empty_domain(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let (log, blocked) = fault_boot(test_config, c_bins, rust_bins, &["iommu-empty-domain"])?;

    let nvme = class_function(&log, "0108").ok_or_else(|| {
        format!("this machine enumerated no NVMe controller to strand\n{}", log.text())
    })?;
    if blocked.stream != nvme {
        return Err(format!(
            "the unit blocked {} but the controller given an empty domain is {nvme}",
            blocked.stream
        ));
    }
    if !SECOND_LEVEL.contains(&blocked.reason.as_str()) {
        return Err(format!(
            "the unit blocked {nvme} for {:?}, which is not something a second-level page table \
             walk decides. A present context entry over an empty domain has to be refused by the \
             walk itself — any other reason means the unit stopped before it, and a context entry \
             naming passthrough would not have walked at all",
            blocked.reason
        ));
    }
    // Inside the memory the identity domain covers, because that is where the
    // driver's descriptors are: a fault somewhere else would be a different
    // machine's bug wearing this one's clothes.
    let covered = identity_extent(&log)?;
    let at = u64::from_str_radix(blocked.address.trim_start_matches("0x"), 16)
        .map_err(|_| format!("unreadable faulting address {:?}", blocked.address))?;
    if at == 0 || at >= covered {
        return Err(format!(
            "the unit blocked an access to {}, and the driver's descriptors are inside \
             0x0..{covered:#x}",
            blocked.address
        ));
    }
    eprintln!(
        "  [iommu] {nvme} given an empty domain: blocked at {} on a {} for {}",
        blocked.address, blocked.access, blocked.reason
    );
    Ok(())
}

/// A device with an address space of its own reaches what is in it and faults
/// on everything else, and the memory it was aimed at is untouched.
///
/// The actuator points the NIC's first RX buffer at the physical bytes NVMe's
/// admin completion queue page ends with — a page the NIC's domain does not map.
/// Three things then hold at once, and no two come from the same place: the unit
/// blocks it and names the NIC and that address; the address is the one NVMe's
/// own `ACQ` register holds, resolved through the tables the unit walks rather
/// than taken off a console line; and every byte of the 2 KiB is still zero.
///
/// Oracle: VT-d Rev. 4.0 Section 9.8, which [`translate`] implements
/// independently, and QEMU's `vtd_iova_to_sspte`
/// (`hw/i386/intel_iommu.c:1146-1210` at v11.1.0).
pub fn iommu_domain_isolation(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
) -> Result<(), String> {
    let options = BootOptions {
        profile: Profile::Headless,
        qmp: true,
        kernel_params: &["iommu-nic-foreign-dma"],
        ready_marker: FAULT,
        ..Default::default()
    };
    unit_is_first(&qemu::profile_argv(&options), "isolation")?;
    let mut qemu = QemuInstance::boot_with_options(test_config, c_bins, rust_bins, options);
    let mut log = Serial::boot(&qemu);
    log.push(&qemu.drain_serial(Duration::from_secs(2)));
    let socket = qemu.qmp_socket();

    let line = log.must_say(FAULT)?;
    let fields = unit_fields(line);
    let field = |k: &str| -> Result<String, String> {
        fields.get(k).cloned().ok_or_else(|| format!("the fault line has no {k}=: {line:?}"))
    };
    let blocked = Blocked {
        stream: field("stream")?,
        address: field("addr")?,
        access: field("access")?,
        reason: line
            .split_whitespace()
            .last()
            .ok_or_else(|| format!("the fault line names no reason: {line:?}"))?
            .to_string(),
    };

    let nic = class_function(&log, "0200")
        .ok_or_else(|| format!("this machine enumerated no NIC\n{}", log.text()))?;
    let nvme = class_function(&log, "0108")
        .ok_or_else(|| format!("this machine enumerated no NVMe controller\n{}", log.text()))?;
    if blocked.stream != nic {
        return Err(format!(
            "the unit blocked {} and the device aimed at another driver's pool is {nic}",
            blocked.stream
        ));
    }
    if !SECOND_LEVEL.contains(&blocked.reason.as_str()) {
        return Err(format!(
            "the unit blocked {nic} for {:?}, which is not something a second-level page table \
             walk decides — a domain that does not map an address has to refuse it in the walk",
            blocked.reason
        ));
    }

    // The victim's address out of the victim's own register and the unit's own
    // tables: `ACQ` is what NVMe programmed, in whatever space NVMe is in.
    let window = register_window(socket, &log, "isolation")?;
    let acq = over_qmp(socket, nvme_bar(socket, &log, &nvme)? + NVME_ACQ, 1, 'g')?[0];
    let victim = translate(socket, window, &nvme, acq)?;
    let at = u64::from_str_radix(blocked.address.trim_start_matches("0x"), 16)
        .map_err(|_| format!("unreadable faulting address {:?}", blocked.address))?;
    if at != victim & !0xFFF {
        return Err(format!(
            "the unit blocked an access to {}, and the page NVMe's ACQ names — through the \
             tables the unit walks — is {:#x}. The kernel is not reporting the address the \
             device was aimed at",
            blocked.address,
            victim & !0xFFF
        ));
    }

    let probe = victim + 0x800;
    let words = over_qmp(socket, probe, PROBE_WORDS, 'g')?;
    if let Some((i, word)) = words.iter().enumerate().find(|(_, w)| **w != 0) {
        return Err(format!(
            "the unit reported blocking {nic} at {}, and the {} bytes at {probe:#x} inside \
             NVMe's pool hold {word:#018x} at word {i} rather than the zero the NVMe driver \
             left. The write landed anyway",
            blocked.address,
            PROBE_WORDS * 8
        ));
    }
    // Out of the function's own `COMMAND` rather than off the line the handler printed.
    let command = over_qmp(socket, config_space(&log, &nic)? + PCI_COMMAND, 1, 'w')?[0] as u16;
    if command & PCI_BUS_MASTER != 0 {
        return Err(format!(
            "the unit blocked {nic} and its COMMAND is {command:#06x}, so it still masters the \
             bus and can fault again"
        ));
    }
    let handled = log.must_say(FAULT)?;
    let nic_domain = context_of(socket, window, &nic)?.0;
    for field in [
        "bme=cleared".to_string(),
        "first=y".to_string(),
        format!("domain={nic_domain}"),
        "unitfaults=1".to_string(),
        "streamfaults=1".to_string(),
    ] {
        if !handled.contains(&field) {
            return Err(format!("the fault line does not say {field}: {handled:?}"));
        }
    }
    eprintln!(
        "  [iommu] {nic} aimed at {}, inside {nvme}'s pool: blocked on a {} for {}, all {} bytes \
         there are still zero, and its COMMAND reads {command:#06x} — bus mastering gone",
        blocked.address,
        blocked.access,
        blocked.reason,
        PROBE_WORDS * 8
    );
    // The first guest goes first: QEMU holds an exclusive lock on the NVMe image.
    let lane = qemu.shutdown();
    let clean =
        QemuInstance::boot_with_options(test_config, c_bins, rust_bins, BootOptions {
            profile: Profile::Headless,
            qmp: true,
            ..Default::default()
        });
    drop(lane);
    let log = Serial::boot(&clean);
    log.must_be_clean()?;
    log.must_say("Boot: complete")?;
    let socket = clean.qmp_socket();
    let window = register_window(socket, &log, "isolation")?;
    let nvme = class_function(&log, "0108")
        .ok_or_else(|| format!("this machine enumerated no NVMe controller\n{}", log.text()))?;
    let acq = over_qmp(socket, nvme_bar(socket, &log, &nvme)? + NVME_ACQ, 1, 'g')?[0];
    let owned = translate(socket, window, &nvme, acq)?;
    domains_are_disjoint(socket, &log, window, owned, &nvme)
}

/// Every moved function is in a domain of its own, and no two of those domains
/// reach the same physical page. A set comparison and not a spot check: a
/// domain's addresses start at `1 << (width - 2)`, so asking whether one
/// translates an address inside RAM misses every populated top-level index by
/// construction and cannot fail.
fn domains_are_disjoint(
    socket: &Path,
    log: &Serial,
    window: u64,
    owned: u64,
    owner: &str,
) -> Result<(), String> {
    let mut seen: BTreeMap<String, u64> = BTreeMap::new();
    for line in log.text().lines() {
        let Some(rest) = line.split("iommu: ").nth(1) else { continue };
        let Some((bdf, tail)) = rest.split_once(' ') else { continue };
        let Some(id) = tail.strip_prefix("moves to domain") else { continue };
        let id: u64 = id.trim().parse().map_err(|_| format!("unreadable domain on {line:?}"))?;
        if seen.insert(bdf.to_string(), id).is_some() {
            return Err(format!("{bdf} moved twice: {line:?}"));
        }
    }
    if seen.len() < 2 {
        return Err(format!(
            "{} function(s) moved to a domain of their own, so there is no pair here to be \
             disjoint\n{}",
            seen.len(),
            log.text()
        ));
    }

    let mut roots: BTreeMap<u64, String> = BTreeMap::new();
    let mut pages: BTreeMap<String, BTreeSet<u64>> = BTreeMap::new();
    for (bdf, want) in &seen {
        let (did, root, levels) = context_of(socket, window, bdf)?;
        if did != *want {
            return Err(format!(
                "the kernel says {bdf} is in domain {want} and its context entry names domain \
                 {did}"
            ));
        }
        if let Some(other) = roots.insert(root, bdf.clone()) {
            return Err(format!(
                "{bdf} and {other} name the same second-level table at {root:#x}, so they are \
                 one address space wearing two domain ids"
            ));
        }
        let mine = leaves(socket, root, levels, bdf)?;
        if mine.is_empty() {
            return Err(format!("{bdf}'s domain {did} maps nothing at all"));
        }
        if mine.contains(&(owned & !(PAGE_2M - 1))) != (bdf == owner) {
            return Err(format!(
                "{bdf}'s domain {did} maps the page at {owned:#x} = {}, and that page is \
                 {owner}'s admin completion queue",
                mine.contains(&(owned & !(PAGE_2M - 1)))
            ));
        }
        for (other, theirs) in &pages {
            if let Some(shared) = mine.intersection(theirs).next() {
                return Err(format!(
                    "{bdf}'s domain and {other}'s both map the physical page at {shared:#x}, so \
                     either can reach what the other was given"
                ));
            }
        }
        pages.insert(bdf.clone(), mine);
    }
    eprintln!(
        "  [iommu] {} function(s) in {} domains over {} distinct second-level tables, mapping {} \
         pairwise-disjoint 2 MiB pages, and only {owner} maps {owned:#x}: {seen:?}",
        seen.len(),
        seen.values().collect::<BTreeSet<_>>().len(),
        roots.len(),
        pages.values().map(BTreeSet::len).sum::<usize>()
    );
    Ok(())
}

/// Every physical page one domain's second-level tables reach, Section 9.8's
/// walk, a whole 4 KiB of entries at a time.
fn leaves(socket: &Path, root: u64, levels: u64, bdf: &str) -> Result<BTreeSet<u64>, String> {
    let mut found = BTreeSet::new();
    let mut level = levels;
    let mut tables = BTreeSet::from([root]);
    while level > 2 {
        let mut next = BTreeSet::new();
        for table in &tables {
            for entry in over_qmp(socket, *table, ENTRIES, 'g')? {
                if entry & 0x3 == 0 {
                    continue;
                }
                // A 1 GiB leaf followed as a pointer reads 512 words out of a data page.
                if entry & LARGE_PAGE != 0 {
                    return Err(format!(
                        "{bdf}: a level-{level} entry {entry:#018x} carries the page-size bit, \
                         and this kernel writes only 2 MiB leaves"
                    ));
                }
                next.insert(entry & ENTRY_ADDR);
            }
        }
        tables = next;
        level -= 1;
    }
    for table in &tables {
        for entry in over_qmp(socket, *table, ENTRIES, 'g')? {
            if entry & 0x3 == 0 {
                continue;
            }
            if entry & LARGE_PAGE == 0 {
                return Err(format!(
                    "{bdf}: a page-directory entry {entry:#018x} without the page-size bit, and \
                     this kernel writes only 2 MiB leaves"
                ));
            }
            found.insert(entry & ENTRY_ADDR & !(PAGE_2M - 1));
        }
    }
    Ok(found)
}

/// Entries in one 4 KiB second-level table, read in a single monitor command.
const ENTRIES: usize = 512;
/// Section 9.8: bit 7 of a page-directory entry, a 2 MiB leaf rather than a pointer.
const LARGE_PAGE: u64 = 1 << 7;

/// One function's context entry, as `(DID, second-level root, levels)`; Section
/// 9.3 puts `DID` at 87:72, `SLPTPTR` at 51:12 and `AW` at 66:64 as levels minus two.
fn context_of(socket: &Path, window: u64, bdf: &str) -> Result<(u64, u64, u64), String> {
    let (bus, dev, func) = parse_bdf(bdf)?;
    let root = over_qmp(socket, window + RTADDR_REG, 1, 'g')?[0] & ENTRY_ADDR;
    let entry = over_qmp(socket, root + u64::from(bus) * 16, 1, 'g')?[0];
    if entry & 1 == 0 {
        return Err(format!("{bdf}: the root entry for bus {bus:#04x} is not present"));
    }
    let devfn = u64::from(dev) * 8 + u64::from(func);
    let context = over_qmp(socket, (entry & ENTRY_ADDR) + devfn * 16, 2, 'g')?;
    if context[0] & 1 == 0 {
        return Err(format!("{bdf}: its context entry is not present"));
    }
    Ok(((context[1] >> 8) & 0xFFFF, context[0] & ENTRY_ADDR, (context[1] & 0x7) + 2))
}

/// `COMMAND` and its Bus Master Enable bit, PCI 3.0 §6.2.2.
const PCI_COMMAND: u64 = 0x04;
const PCI_BUS_MASTER: u16 = 0x04;

/// One function's config space in ECAM.
fn config_space(log: &Serial, bdf: &str) -> Result<u64, String> {
    let line = log.must_say("ACPI: ECAM base address: ")?;
    let ecam = line
        .split("ACPI: ECAM base address: 0x")
        .nth(1)
        .and_then(|hex| u64::from_str_radix(hex.trim(), 16).ok())
        .ok_or_else(|| format!("unreadable ECAM base on {line:?}"))?;
    let (bus, dev, func) = parse_bdf(bdf)?;
    Ok(ecam + (u64::from(bus) << 20) + (u64::from(dev) << 15) + (u64::from(func) << 12))
}

/// The untouched half of NVMe's admin completion queue page: a frame is 1526
/// bytes at most, so this covers the whole of one landing there.
const PROBE_WORDS: usize = 256;

/// `REG_ACQ`, NVMe 2.0 Figure 41.
const NVME_ACQ: u64 = 0x30;

/// Where QEMU puts an `intel-iommu` on q35, which every profile here is: a
/// constant on the host side, not a number the guest supplies.
///
/// `Q35_HOST_BRIDGE_IOMMU_ADDR`, `include/hw/i386/intel_iommu.h:35` at v11.1.0,
/// mapped at `intel_iommu.c:5635`. The DMAR the guest reads is built from that
/// same constant (`acpi-build.c:1687`), so this is one source agreeing with
/// itself and not two: what it catches is a guest reporting something else.
const UNIT_WINDOW: u64 = 0xfed9_0000;

/// The `-device` name of the audio function that is ruled to stay outside the unit.
const SOUND: &str = "virtio-sound";

fn unit_is_first(argv: &[String], name: &str) -> Result<(), String> {
    let devices: Vec<&str> =
        argv.windows(2).filter(|w| w[0] == "-device").map(|w| w[1].as_str()).collect();
    let Some(unit) = devices.iter().find(|d| d.starts_with("intel-iommu")) else {
        return Ok(());
    };
    if devices[0] != *unit {
        return Err(format!(
            "{name}: the unit is not the first -device ({} is), so every function ahead of it \
             gets QEMU's bypassing address space",
            devices[0]
        ));
    }
    Ok(())
}

/// The unit's register window, and the one thing on the guest's side of this
/// gate that is checked rather than believed: a kernel that wrote the real
/// `VER`, `GSTS`, `RTADDR` and `IRTA` into a page of RAM and printed *that*
/// address satisfied every readback here. One unit, stated rather than assumed —
/// `must_say` answers with the first match, so a second line is refused.
fn register_window(socket: &Path, log: &Serial, name: &str) -> Result<u64, String> {
    let lines: Vec<&str> =
        log.text().lines().filter(|l| l.contains("translating gsts=")).collect();
    let [line] = lines[..] else {
        return Err(format!(
            "{name}: {} unit(s) are translating and this gate reads one window at \
             {UNIT_WINDOW:#x}; a second needs the harness to model it\n{lines:?}",
            lines.len()
        ));
    };
    let printed = line
        .split(" @")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|hex| u64::from_str_radix(hex.trim_start_matches("0x"), 16).ok())
        .ok_or_else(|| format!("{name}: no register window on {line:?}"))?;
    if printed != UNIT_WINDOW {
        return Err(format!(
            "{name}: the kernel says its unit is at {printed:#x} and QEMU puts one at \
             {UNIT_WINDOW:#x}. Every table this gate walks starts there, so a page of RAM \
             printed here would be a set of forged registers\n{line}"
        ));
    }
    // A unit, not a page somebody left all-ones: `VER` reads a real version,
    // which is the same test the kernel makes before programming it.
    let version = over_qmp(socket, UNIT_WINDOW, 1, 'w')?[0] as u32;
    if version == u32::MAX || (version >> 4) & 0xF == 0 {
        return Err(format!(
            "{name}: {UNIT_WINDOW:#x} reads VER={version:#010x}, so no unit decodes there"
        ));
    }
    Ok(UNIT_WINDOW)
}

/// A function's memory BAR 0, out of ECAM rather than off a console line.
fn nvme_bar(socket: &Path, log: &Serial, bdf: &str) -> Result<u64, String> {
    let config = config_space(log, bdf)?;
    // A window that decodes at all: an ECAM base the kernel invented would read
    // back all ones here, which is no vendor id.
    if over_qmp(socket, config, 1, 'w')?[0] as u32 & 0xFFFF == 0xFFFF {
        return Err(format!("no PCI function decodes at {config:#x}, so that is not ECAM"));
    }
    Ok(over_qmp(socket, config + 0x10, 1, 'g')?[0] & !0xF)
}

fn parse_bdf(bdf: &str) -> Result<(u8, u8, u8), String> {
    let (bus, rest) = bdf.split_once(':').ok_or_else(|| format!("not a bdf: {bdf:?}"))?;
    let (dev, func) = rest.split_once('.').ok_or_else(|| format!("not a bdf: {bdf:?}"))?;
    let refuse = |_| format!("not a bdf: {bdf:?}");
    Ok((
        u8::from_str_radix(bus, 16).map_err(refuse)?,
        u8::from_str_radix(dev, 16).map_err(refuse)?,
        func.parse().map_err(refuse)?,
    ))
}

/// What the unit itself would translate `at` to for `bdf`, decoded here from
/// Sections 9.1, 9.3 and 9.8 out of the tables it really walks: `RTADDR_REG`,
/// then the root entry for the bus, then the context entry for the function,
/// then the second-level tables the context entry names, at the depth its `AW`
/// field declares.
fn translate(socket: &Path, window: u64, bdf: &str, at: u64) -> Result<u64, String> {
    let (bus, dev, func) = parse_bdf(bdf)?;
    let root = over_qmp(socket, window + RTADDR_REG, 1, 'g')?[0] & ENTRY_ADDR;
    let entry = over_qmp(socket, root + u64::from(bus) * 16, 1, 'g')?[0];
    if entry & 1 == 0 {
        return Err(format!("{bdf}: the root entry for bus {bus:#04x} is not present"));
    }
    let devfn = u64::from(dev) * 8 + u64::from(func);
    let context = over_qmp(socket, (entry & ENTRY_ADDR) + devfn * 16, 2, 'g')?;
    if context[0] & 1 == 0 {
        return Err(format!("{bdf}: its context entry is not present"));
    }
    // `AW` is levels minus two, Section 9.3.
    let mut level = (context[1] & 0x7) + 2;
    let mut table = context[0] & ENTRY_ADDR;
    while level > 2 {
        let index = (at >> (12 + 9 * (level - 1))) & 0x1FF;
        let next = over_qmp(socket, table + index * 8, 1, 'g')?[0];
        if next & 0x3 == 0 {
            return Err(format!("{bdf}: {at:#x} has no level-{level} entry"));
        }
        table = next & ENTRY_ADDR;
        level -= 1;
    }
    let leaf = over_qmp(socket, table + ((at >> 21) & 0x1FF) * 8, 1, 'g')?[0];
    if leaf & 0x3 == 0 {
        return Err(format!("{bdf}: {at:#x} has no leaf"));
    }
    Ok((leaf & ENTRY_ADDR & !(PAGE_2M - 1)) | (at & (PAGE_2M - 1)))
}

/// What the unit reported when it blocked a transaction.
struct Blocked {
    stream: String,
    address: String,
    access: String,
    reason: String,
}

/// Boot a deliberately mis-programmed machine and read the first fault off it.
///
/// The fault line is the ready marker, so a boot that never produces one fails
/// as a boot timeout — which is exactly what a unit that is not translating
/// would do, and is why neither gate can pass vacuously.
fn fault_boot(
    test_config: &Path,
    c_bins: &[(String, Vec<u8>)],
    rust_bins: &[(String, Vec<u8>)],
    params: &'static [&'static str],
) -> Result<(Serial, Blocked), String> {
    let mut qemu = QemuInstance::boot_with_options(
        test_config,
        c_bins,
        rust_bins,
        BootOptions {
            profile: Profile::Metal,
            kernel_params: params,
            ready_marker: FAULT,
            ..Default::default()
        },
    );
    let mut log = Serial::boot(&qemu);
    // Past the fault, because the claim is that the machine stopped there: the
    // handler halts every CPU, so anything the boot would have gone on to do
    // has to be absent from a window that stays open after it.
    log.push(&qemu.drain_serial(Duration::from_secs(2)));
    log.must_not_say("Boot: complete")?;
    log.must_not_say(qemu::DEFAULT_READY)?;

    let line = log.must_say(FAULT)?;
    let fields = unit_fields(line);
    let field = |k: &str| -> Result<String, String> {
        fields.get(k).cloned().ok_or_else(|| format!("the fault line has no {k}=: {line:?}"))
    };
    let reason = line
        .split_whitespace()
        .last()
        .ok_or_else(|| format!("the fault line names no reason: {line:?}"))?
        .to_string();
    if reason == "unnamed" {
        return Err(format!(
            "the unit reported a fault reason this kernel has no name for: {line:?}"
        ));
    }
    let blocked = Blocked {
        stream: field("stream")?,
        address: field("addr")?,
        access: field("access")?,
        reason,
    };
    Ok((log, blocked))
}

/// How far up the identity domain reaches, off the line that built it.
fn identity_extent(log: &Serial) -> Result<u64, String> {
    let line = log.must_say("iommu: identity domain")?;
    let range = line
        .split_whitespace()
        .find(|w| w.starts_with("0x0.."))
        .ok_or_else(|| format!("no extent on {line:?}"))?;
    let top = range.trim_start_matches("0x0..0x");
    u64::from_str_radix(top, 16).map_err(|_| format!("unreadable extent on {line:?}"))
}

/// The one function `pci::enumerate` printed with this class, or none.
///
/// `None` rather than a first match over several: two controllers of one class
/// would make "the one the actuator skipped" ambiguous, and a gate that picked
/// either would be asserting against a guess.
fn class_function(log: &Serial, class: &str) -> Option<String> {
    let mut found: Option<String> = None;
    for line in log.text().lines() {
        let Some((bdf, tail)) = line.split("PCI ").nth(1).and_then(|r| r.split_once(' ')) else {
            continue;
        };
        if tail.starts_with(&format!("[{class}]")) {
            if found.is_some() {
                return None;
            }
            found = Some(bdf.to_string());
        }
    }
    found
}

/// The `key=value` pairs on a unit line. `@0xfed90000` carries no `=` and is
/// skipped, which is what makes the split total rather than a parse.
fn unit_fields(line: &str) -> BTreeMap<String, String> {
    line.split_whitespace()
        .filter_map(|word| word.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn expect(got: &str, want: &str, key: &str, name: &str, line: &str) -> Result<(), String> {
    if got == want {
        return Ok(());
    }
    Err(format!("{name}: {key}={got}, want {key}={want}\n{line}"))
}

/// The requester ids the unit's scopes name are exactly the functions this
/// machine enumerated. Returns how many.
///
/// Set equality rather than "each one exists", and the difference is the whole
/// value of this check. Measured against the raw table on QEMU 11.0.2: the
/// DRHD carries no `INCLUDE_PCI_ALL` flag and instead lists every PCI function
/// as its own scope, so the two sets are the same set. A path read one byte
/// off produces ids that are still *plausible* — `00:1f.3` becomes `00:03.0`,
/// which on this machine is the NVMe controller — and an each-one-exists check
/// stays green on all seven of them. The set catches it, because five of the
/// seven collapse onto `00:00.0` and four real functions go missing.
///
/// A failure here on a future QEMU that switches to `INCLUDE_PCI_ALL` is a
/// real report and not a false one: which functions a unit's scope names is
/// what stage I2 hands context entries to.
fn scope_check(log: &Serial, name: &str) -> Result<usize, String> {
    let mut scoped: Vec<String> = Vec::new();
    for line in log.text().lines() {
        let Some(rest) = line.split("iommu: unit0 scope ").nth(1) else { continue };
        let mut words = rest.split_whitespace();
        let (Some(kind), Some(who)) = (words.next(), words.next()) else {
            return Err(format!("{name}: unreadable scope line: {line:?}"));
        };
        // An I/O APIC sits on a pseudo-bus no PCI walk sees, and a scope whose
        // path runs through a bridge reports no requester id at all — neither
        // is a name this cross-check can look up.
        if kind == "pci-endpoint" || kind == "pci-bridge" {
            scoped.push(who.to_string());
        }
    }

    let unique: BTreeSet<&String> = scoped.iter().collect();
    if unique.len() != scoped.len() {
        return Err(format!(
            "{name}: the unit names {} scopes but only {} distinct requester ids. A unit cannot \
             name the same requester twice, so the path bytes are being read at the wrong \
             offset: {scoped:?}",
            scoped.len(),
            unique.len()
        ));
    }

    let enumerated = enumerated_functions(log);
    let scoped: BTreeSet<String> = scoped.into_iter().collect();
    if scoped != enumerated {
        return Err(format!(
            "{name}: the unit's scope names {scoped:?} and this machine enumerated \
             {enumerated:?}. On QEMU these are the same set — the DRHD lists every function \
             rather than setting INCLUDE_PCI_ALL."
        ));
    }
    if scoped.is_empty() {
        return Err(format!(
            "{name}: neither the unit nor the PCI walk named a single function, so this \
             comparison is between two empty sets"
        ));
    }
    Ok(scoped.len())
}

/// Every function `pci::enumerate` printed. Anchored on the class field that
/// follows the address, so `xHCI: found at PCI 00:02.0` is not one of them.
fn enumerated_functions(log: &Serial) -> BTreeSet<String> {
    log.text()
        .lines()
        .filter_map(|line| {
            let (bdf, tail) = line.split("PCI ").nth(1)?.split_once(' ')?;
            tail.starts_with('[').then(|| bdf.to_string())
        })
        .collect()
}

fn profile_name(profile: Profile) -> &'static str {
    match profile {
        Profile::Metal => "metal",
        Profile::NoIommu => "no-iommu",
        Profile::IommuNarrow => "narrow",
        Profile::IommuNoIntremap => "no-intremap",
        Profile::IommuEim => "eim",
        _ => "unexpected",
    }
}

/// Presence, configuration and *position* of the unit in the argv.
///
/// The last one is the vacuity trap in its harness-side form: QEMU hands a PCI
/// function the bypassing
/// address space when the function is created before the unit exists, so a
/// `-device intel-iommu` emitted after the devices it is meant to decode is a
/// unit that decodes nothing — and every assertion above it would still pass.
fn argv_check(profile: Profile, argv: &[String]) -> Result<(), String> {
    let name = profile_name(profile);
    let devices: Vec<&str> = argv
        .windows(2)
        .filter(|w| w[0] == "-device")
        .map(|w| w[1].as_str())
        .collect();
    let unit = devices.iter().find(|d| d.starts_with("intel-iommu"));
    let machine = argv
        .windows(2)
        .find(|w| w[0] == "-machine")
        .map(|w| w[1].as_str())
        .ok_or_else(|| format!("{name}: no -machine in the argv"))?;

    match profile.iommu() {
        None => {
            if let Some(d) = unit {
                return Err(format!("{name} declares no unit but QEMU is given {d}"));
            }
            if machine.contains("kernel-irqchip") {
                return Err(format!(
                    "{name} declares no unit but the machine is still split-irqchip: {machine}"
                ));
            }
        }
        Some(want) => {
            let d = *unit.ok_or_else(|| {
                format!("{name} declares a unit and QEMU is given none: {devices:?}")
            })?;
            for field in [
                format!("aw-bits={}", want.aw_bits),
                format!("intremap={}", if want.intremap { "on" } else { "off" }),
                format!("eim={}", if want.eim { "on" } else { "off" }),
                String::from("caching-mode=on"),
            ] {
                if !d.contains(&field) {
                    return Err(format!("{name}: {field} is not in {d}"));
                }
            }
            if !machine.contains("kernel-irqchip=split") {
                return Err(format!(
                    "{name}: interrupt remapping needs the userspace half of the irqchip, and \
                     the machine is {machine}"
                ));
            }
            if devices[0] != d {
                return Err(format!(
                    "{name}: the unit is not the first -device ({} is), so every function ahead \
                     of it gets QEMU's bypassing address space",
                    devices[0]
                ));
            }
        }
    }
    Ok(())
}
