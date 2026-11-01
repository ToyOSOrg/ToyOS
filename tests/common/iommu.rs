//! `iommu_virtio_platform`: whether each virtio function the kernel drives
//! itself negotiated `VIRTIO_F_ACCESS_PLATFORM` behind QEMU's emulated VT-d
//! unit and none without one, and that a process's claim on one is handed
//! over behind the unit and refused where no unit is.
//! `amdvi_firmware_left`: whether an AMD-Vi unit firmware left blocking is
//! switched off before the boot stick is driven.
//! `vtd_firmware_left`: whether a VT-d unit firmware left translating, with no
//! protected memory regions, goes off before it is programmed.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::qemu::{self, is_kernel_line, BootOptions, Profile, QemuInstance};
use super::serial::Serial;

/// The function `tests/images/netcase.toml`'s virtio-net row drives, as the row spells it.
const NIC_CLAIMS: &str = "1af4:1041";

/// The boot config that runs `netstack` with a virtio NIC in front of it.
fn netcase() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/images/netcase.toml")
}

/// Whether this machine's virtio functions are behind the unit at all.
///
/// QEMU keeps a virtio function on `&address_space_memory` — the unit bypassed,
/// whatever the tables say — unless it is created with `iommu_platform=on`
/// (`hw/virtio/virtio-bus.c:86-99`, `hw/virtio/virtio-pci.c:1400-1405` at
/// v11.1.1), and under identity mapping the two are indistinguishable. So the
/// argv says which functions were created behind a unit and the kernel's
/// records say which of those it drives negotiated `VIRTIO_F_ACCESS_PLATFORM`;
/// the NIC's and the sound function's drivers are processes', whose
/// negotiation is `toyos-virtio`'s and host-tested there. On [`Profile::HeadlessNoIommu`]
/// the guest reports `n` because QEMU never *offers* the bit
/// (`hw/virtio/virtio-bus.c:87-94`), not because the driver declined.
pub fn iommu_virtio_platform() -> Result<(), String> {
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
            if device.contains("iommu_platform=on") != behind_unit {
                return Err(format!(
                    "{name}: the machine has a unit = {behind_unit} and QEMU is given {device}, \
                     where iommu_platform=on is owed = {behind_unit}. A virtio function without \
                     it keeps the address space the unit does not decode"
                ));
            }
        }

        // `netcase`: the NIC's driver is a process, and this is the one config
        // that runs it. The kernel's records of a claim are waited for on the
        // stream: the boot log ends at test-runner's `===READY===`, its first
        // act, and nothing orders that against the claims of the programs the
        // supervisor started before it.
        let mut qemu = QemuInstance::boot_with_options(&netcase(), &[], &[], options);
        let said: Vec<String> = if behind_unit {
            vec![bar_handed(), msix_armed()]
        } else {
            // The NIC's driver is a `drives` row, which the supervisor
            // starts after every service: its refusal is the boot's last.
            vec![
                not_handed_over(CLAIMED_AT, NOT_REMAPPED),
                not_handed_over(NVME_AT, NOT_REMAPPED),
                not_handed_over(SOUND_AT, NOT_REMAPPED),
                not_handed_to_its_driver(NIC_CLAIMS),
            ]
        };
        let mut text = qemu.boot_log().to_string();
        qemu::await_guest(&mut qemu, &mut text, &format!("{name}: {said:?}"), |t| {
            said.iter().all(|line| t.contains(line))
        })
        .map_err(|e| format!("{e}\n{text}"))?;
        let log = Serial::named("boot console", text);
        log.must_be_clean()?;
        log.must_say("Boot: complete")?;
        // The CPU's own source in the generator's key, which firmware's seed
        // alone would key without it wherever firmware answers EFI_RNG_PROTOCOL.
        log.must_say("random: RDRAND is mixed into the generator's key")?;

        // **Whether the NIC's function is handed to a process at all is the
        // machine's answer, not a choice.** A process driving a device writes
        // addresses into descriptors, so the substrate refuses a claim on a
        // function this machine cannot give an address space of its own — and
        // on a machine with no unit that is every function.
        if behind_unit {
            // The two things a hand-over spends, on the same function and the
            // same machine the arm below requires to be unspent. Without this
            // pair those `must_not_say`s would pass against a kernel that had
            // stopped writing either line.
            log.must_say(&bar_handed())?;
            log.must_say(&msix_armed())?;
        } else {
            no_unit_is_no_claim(&log)?;
        }

        // Every function the kernel drives itself: all QEMU created but the
        // NIC, the sound function and the clipboard's channel, whose drivers
        // are processes.
        let expected = created.len() - 3;
        let mut negotiated = Vec::new();
        for line in log.text().lines().filter(|line| is_kernel_line(line)) {
            let Some(rest) = line.split("VirtIO: PCI ").nth(1) else { continue };
            let Some((who, _)) = rest.split_once(' ') else {
                return Err(format!("{name}: unreadable feature line: {line:?}"));
            };
            let fields = unit_fields(line);
            let Some(accepted) = fields.get("access_platform") else { continue };
            negotiated.push((who.to_string(), accepted == "y"));
        }
        if negotiated.len() != expected {
            return Err(format!(
                "{name}: QEMU created {} virtio function(s), {expected} of them for the kernel's \
                 own drivers, and the kernel negotiated features with {} — \
                 {negotiated:?} against {created:?}",
                created.len(),
                negotiated.len()
            ));
        }
        let enumerated = enumerated_functions(&log);
        for (who, accepted) in &negotiated {
            if *accepted != behind_unit {
                return Err(format!(
                    "{name}: {who} negotiated VIRTIO_F_ACCESS_PLATFORM = {accepted} where \
                     {behind_unit} is owed — the unit exists = {behind_unit}"
                ));
            }
            if !enumerated.contains(who) {
                return Err(format!(
                    "{name}: {who} negotiated features and the PCI walk enumerated \
                     {enumerated:?} — the driver is naming a function this machine does not have"
                ));
            }
        }
        eprintln!(
            "  [iommu] {name}: the kernel's {} virtio function(s) negotiated, behind a unit = {behind_unit}; {}",
            negotiated.len(),
            if behind_unit { "the NIC's claim handed over" } else { "every claim refused for want of a unit" }
        );
    }
    Ok(())
}

/// The slot QEMU's `-device` order puts `tests/images/netcase.toml`'s NVMe controller on,
/// the one its nvme row drives.
const NVME_AT: &str = "00:02.0";

/// The slot QEMU's `-device` order puts the boot controller on, the one the
/// image's usbd row drives.
const XHCI_AT: &str = "00:01.0";

/// The slot QEMU's `-device` order puts the virtio-sound function on, the one
/// `tests/images/netcase.toml`'s soundserver row claims.
const SOUND_AT: &str = "00:04.0";

/// Why a machine with no unit hands no function over, in the kernel's words.
const NOT_REMAPPED: &str = "its interrupts would not be remapped on this machine";

/// The kernel's record of a claim on the function at `at` refused for `why`.
fn not_handed_over(at: &str, why: &str) -> String {
    format!("pcidev: PCI {at} NOT HANDED OVER — {why}")
}

/// **A machine with no unit hands no function to a process**, and the
/// supervisor that asked is told so.
///
/// The ordering ruling this whole stage stands on
/// (`issues/every-driver-is-still-in-the-kernel.md`) is that moving a
/// driver out without the unit costs security: a message nothing remaps raises
/// any vector on any CPU, and a descriptor holding a physical address is an
/// arbitrary read and write over all of memory. So the kernel refuses the
/// claim by name and the supervisor says which device it could not mint — and
/// the machine finishes booting, which is the half a refusal that panicked
/// would fail. The NVMe controller and the virtio-sound function are refused
/// the same.
fn no_unit_is_no_claim(log: &Serial) -> Result<(), String> {
    refused_claim(log, NIC_CLAIMS, NOT_REMAPPED, &[XHCI_AT, NVME_AT, SOUND_AT])?;
    log.must_say(&not_handed_over(XHCI_AT, NOT_REMAPPED))?;
    log.must_say(&format!(
        "supervisor: usbd was not started on PCI {XHCI_AT}: pci:0c:03:30/1033:0194 is on this machine and could not be handed over"
    ))?;
    log.must_say(&not_handed_over(SOUND_AT, NOT_REMAPPED))?;
    log.must_say("supervisor: soundserver: pci:04:01:00/1af4:1059 is on this machine and could not be handed over")?;
    log.must_say(&not_handed_over(NVME_AT, NOT_REMAPPED))?;
    log.must_say(&format!(
        "supervisor: nvme was not started on PCI {NVME_AT}: pci:01:08:02 is on this machine and could not be handed over"
    ))?;
    // And this machine handed *nothing* over, which is more than the claim's
    // own refusal says: with no unit there is no function any process could be
    // given an address space for.
    log.must_not_say("] handed over, ")?;
    Ok(())
}

/// The slot QEMU's `-device` order puts the function virtio-net drives on, and
/// the address every judge below is an assertion about.
///
/// **The address is the harness's own and never the guest's.** A judge that
/// reads the function out of the console and then asserts about *that* asserts
/// about whichever function the kernel happened to name; what the guest printed
/// is asserted equal to this instead, so a constant that names the wrong slot
/// reds and never passes.
const CLAIMED_AT: &str = "00:03.0";

/// The two lines a hand-over of that function spends. One arm requires them and
/// [`refused_claim`] requires their absence, and both read them here: a kernel
/// that stopped writing either line would otherwise satisfy both.
fn bar_handed() -> String {
    format!("pcidev: PCI {CLAIMED_AT} BAR")
}

fn msix_armed() -> String {
    format!("PCI {CLAIMED_AT}: msix address=")
}

/// The older mechanism taken where the newer one was published — required
/// absent by [`refused_claim`].
fn msi_armed() -> String {
    format!("PCI {CLAIMED_AT}: msi address=")
}

/// Every function named by a line carrying `marker`, in the kernel's own
/// spelling.
///
/// **A line that carries the marker and no `pcidev: PCI ` prefix is an error,
/// never a dropped line.** A scan closes only the spellings it matches, so a
/// caller asking what a console named on *every* such line would otherwise be
/// answered about the subset this walk could parse — one refusal read and a
/// second one dropped is the case "and no other function" exists for.
fn functions_named<'a>(log: &'a Serial, marker: &str) -> Result<Vec<&'a str>, String> {
    const PREFIX: &str = "pcidev: PCI ";
    let mut named = Vec::new();
    for line in log.text().lines().filter(|line| line.contains(marker)) {
        named.push(
            line.split(PREFIX)
                .nth(1)
                .and_then(|rest| rest.split_whitespace().next())
                .ok_or_else(|| {
                    format!("{line:?} says {marker:?} and names no function after {PREFIX:?}")
                })?,
        );
    }
    Ok(named)
}

/// **The claim on [`CLAIMED_AT`] was refused for `why`, and the refusal spent
/// nothing**: no BAR of that function was handed over, neither of its two message
/// mechanisms is armed, `claims` reached no holder, and the supervisor said so in the
/// boot config's own spelling. `beside` is every other function this machine
/// refuses, each judged by its own caller.
fn refused_claim(log: &Serial, claims: &str, why: &str, beside: &[&str]) -> Result<(), String> {
    let refused = functions_named(log, "NOT HANDED OVER")?;
    let others: BTreeSet<&str> = refused.iter().copied().filter(|at| *at != CLAIMED_AT).collect();
    if !refused.contains(&CLAIMED_AT) || others != beside.iter().copied().collect() {
        return Err(format!(
            "the claim this judges is the one on {CLAIMED_AT}, beside {beside:?}; this console \
             refused {refused:?}:\n{}",
            log.text()
        ));
    }
    // By the reason true of the path that raised it, on the line that names the
    // function: a refusal whose reason belongs to another path is worse than no
    // line at all.
    log.must_say(&not_handed_over(CLAIMED_AT, why))?;
    log.must_not_say(&format!("[{claims}] handed over"))?;
    log.must_not_say(&msix_armed())?;
    log.must_not_say(&msi_armed())?;
    log.must_not_say(&bar_handed())?;
    // All the way out to userland, rather than a kernel that logged a refusal
    // and handed the NIC's driver its function anyway. The supervisor names
    // what it could not mint in the config's own spelling, and **with this
    // refusal's own word**: the machine has the function, so "no such device on
    // this machine" would be false.
    log.must_say(&not_handed_to_its_driver(claims))?;
    Ok(())
}

/// What the supervisor says of the NIC's claim the kernel would not hand over.
fn not_handed_to_its_driver(claims: &str) -> String {
    format!("supervisor: virtio-net was not started on PCI {CLAIMED_AT}: pci:02:00:00/{claims} is on this machine and could not be handed over")
}


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

/// The `key=value` pairs on a unit line. `@0xfed90000` carries no `=` and is
/// skipped, which is what makes the split total rather than a parse.
pub(crate) fn unit_fields(line: &str) -> BTreeMap<String, String> {
    line.split_whitespace()
        .filter_map(|word| word.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// Every function `pci::enumerate` printed. Anchored on the class field that
/// follows the address, so `pcidev: PCI 00:02.0 [...]` is not one of them.
fn enumerated_functions(log: &Serial) -> BTreeSet<String> {
    log.text()
        .lines()
        .filter_map(|line| {
            let (bdf, tail) = line.split("PCI ").nth(1)?.split_once(' ')?;
            tail.starts_with('[').then(|| bdf.to_string())
        })
        .collect()
}

/// What the kernel says of the one unit QEMU's IVRS describes, as an 11h over
/// a superseded 10h (`lib/acpi/tests/ivrs.rs` decodes the same table).
const AMDVI_UNIT: &str = "iommu: amdvi unit0 @0xfed80000 seg=0 dev=00:02.0 ivhd=0x11";
const AMDVI_SUPERSEDED: &str = "iommu: IVRS IVHD 0x10 at +48 describes 00:02.0 again";
/// The xHCI the boot stick is on, among the functions the unit serves.
const AMDVI_SERVES_XHCI: &str = "iommu: amdvi unit0 serves 00:03.0 data=0x00";
const AMDVI_OFF: &str = "iommu: amdvi unit0 @0xfed80000 switched off control=0x0000000000000000";
/// The unit programmed and on.
const AMDVI_TRANSLATING: &str = "iommu: amdvi unit0 @0xfed80000 translating control=";
/// The boot stick's controller refused to a claim: this unit remaps no
/// message, so no function behind it is handed to a process, and the
/// controller's DMA is no question this machine can answer.
const XHCI_REFUSED: &str = "pcidev: PCI 00:03.0 NOT HANDED OVER — its interrupts would not be remapped on this machine";

/// What the actuator leaves on an AMD-Vi unit, in the kernel's words for
/// `unit`: one line per field the hand-over switches off by its own write.
pub(crate) fn amdvi_left_on(unit: &str) -> [String; 3] {
    ["CmdBufEn", "EventLogEn", "IommuEn"]
        .map(|field| format!("iommu: amdvi {unit} was handed over with {field} on; it goes off"))
}

/// **A VT-d unit left translating, on a unit with no protected memory
/// regions, goes off first and is programmed.** QEMU's unit reports
/// `CAP.PLMR` and `CAP.PHMR` clear, so this is the hand-over without the guard
/// the T14 takes (`iommu_firmware_left`'s row), logged as such. Under
/// `iommu-firmware-left` the unit is left queueing, translating and remapping;
/// each goes off by its own write, the root is set and invalidated through
/// the queue, and the unit ends translating with the boot stick's DMA behind it.
pub fn vtd_firmware_left(test_config: &Path) -> Result<(), String> {
    let options = BootOptions { profile: Profile::Metal, kernel_params: &["iommu-firmware-left"], ..Default::default() };
    let qemu = QemuInstance::boot_with_options(test_config, &[], &[], options);
    let log = Serial::boot(&qemu);
    log.must_be_clean()?;
    let unguarded = log.must_say("iommu: the hand-over is unguarded: unit0 has no protected memory regions")?;
    let off = log.must_say("iommu: unit0 was handed over with translation on; it goes off first")?;
    log.must_say("iommu: unit0 was handed over with queued invalidation on; it goes off first")?;
    log.must_not_say("iommu: protected memory on for")?;
    let translating = log.must_say(" translating gsts=")?;
    if !translating.contains(" tes=y ") {
        return Err(format!("unit0 was programmed and is not translating: {translating:?}"));
    }
    log.must_say("Boot: complete")?;
    eprintln!("  [vtd-firmware-left] {}; {}; {}", unguarded.trim(), off.trim(), translating.trim());
    Ok(())
}

/// **A unit an AMD machine's firmware left on is switched off and
/// programmed.** QEMU's `amd-iommu` is the only AMD-Vi unit the nightly guest
/// suite runs: the T14 is an Intel machine, and the hand-over's register
/// sequence is no host test's. Two boots: the shipped kernel finds a unit
/// nothing left on and switches nothing off; the test kernel's
/// `iommu-firmware-left` leaves the unit blocking every device's DMA with its
/// command buffer and event log running, and each goes off by its own write.
/// With the switch-off reverted that boot reds on the fields' lines and on
/// `control` reading back zero. **The DMA half is the AMD bench's
/// `iommu_firmware_left` row's**: the kernel drives no function behind this
/// unit, and this one remaps no message, so the boot stick's controller is
/// refused to usbd here; QEMU's unit that remaps filters no SMI, which is
/// refused too (`usbd_refused_without_smi_filter`).
pub fn amdvi_firmware_left(test_config: &Path) -> Result<(), String> {
    let profile = Profile::MetalAmdVi;
    let argv = qemu::profile_argv(&BootOptions { profile, ..Default::default() });
    let devices: Vec<&str> = argv.windows(2).filter(|w| w[0] == "-device").map(|w| w[1].as_str()).collect();
    if devices.first() != Some(&"amd-iommu,intremap=off,dma-remap=on") {
        return Err(format!(
            "the AMD unit is not the first -device of {devices:?}, so a function ahead of it bypasses it"
        ));
    }
    for (kernel_params, left) in [(&[][..], false), (&["iommu-firmware-left"][..], true)] {
        let qemu = QemuInstance::boot_with_options(test_config, &[], &[], BootOptions { profile, kernel_params, ..Default::default() });
        let log = Serial::boot(&qemu);
        log.must_be_clean()?;
        log.must_say("Boot: complete")?;
        log.must_say(XHCI_REFUSED)?;
        log.must_say("iommu: IVRS rev=1 ivinfo=0x00002801 efr_images=y preboot_dma_protection=n pa=40 va=0")?;
        log.must_say(AMDVI_UNIT)?;
        log.must_say(AMDVI_SUPERSEDED)?;
        log.must_say(AMDVI_SERVES_XHCI)?;
        log.must_say(AMDVI_OFF)?;
        log.must_say(AMDVI_TRANSLATING)?;
        log.must_not_say("isolates no device")?;
        log.must_not_say("no DMAR or IVRS table")?;
        let handed = log.must_say("iommu: amdvi unit0 handed over control=")?;
        if left {
            // [`amdvi_left_on`]'s fields, as the actuator wrote them.
            if !handed.contains("control=0x0000000000001005") {
                return Err(format!("the actuator's unit was handed over as {handed:?}"));
            }
            for line in amdvi_left_on("unit0") {
                log.must_say(&line)?;
            }
            for field in ["EventIntEn", "GALogEn", "GAIntEn", "PPRLogEn", "PPRIntEn"] {
                log.must_not_say(&format!("with {field} on"))?;
            }
        } else {
            log.must_not_say("was handed over with")?;
        }
        eprintln!("  [amdvi] left on = {left}: {handed}; the stick's controller refused to a claim behind it");
    }
    Ok(())
}

/// **A translating AMD-Vi unit refuses what no domain maps, and the handler
/// reads its record.** `amdvi-selftest` (`kernel/src/arch/x86_64/amdvi/selftest.rs`)
/// on QEMU's `amd-iommu` with two `iommu-testdev` behind it: the first, on a
/// domain of its own, lands a write where that domain maps it; the second,
/// on the identity domain, lands one in memory and is refused one where the
/// identity domain maps nothing, recorded as the kernel's, and the machine
/// halts. The record, not the device's answer, says the unit translated: a
/// write passed through to 2^46 reaches nothing on this 40-bit guest and the
/// device answers it refused either way. QEMU 11.1.1 passes a function
/// through until an `INVALIDATE_DEVTAB_ENTRY` names it, so with the
/// per-function invalidations at boot removed no record is read and this
/// reds. A kernel-owned record halts, so no metal row reaches the event log
/// of a unit that translates for the kernel alone.
pub fn amdvi_selftest(test_config: &Path) -> Result<(), String> {
    let halted = |l: &str| l.contains("panic: rebooting") || l.contains("panic: holding this panel");
    let options = BootOptions {
        profile: Profile::MetalAmdViTestdev,
        kernel_params: &["amdvi-selftest"],
        ready_marker: "iommu: IVRS rev=",
        ..Default::default()
    };
    let argv = qemu::profile_argv(&options);
    if argv.iter().filter(|a| a.as_str() == "iommu-testdev").count() != 2 {
        return Err(format!("the machine has not two iommu-testdevs: {argv:?}"));
    }
    let mut qemu = QemuInstance::boot_with_options(test_config, &[], &[], options);
    let rest = qemu.drain_until(std::time::Duration::from_secs(30), halted);
    let serial = format!("{}\n{rest}", qemu.boot_log());
    let lines: Vec<&str> = serial.lines().collect();
    let at = |want: &str| lines.iter().position(|l| l.contains(want));
    if let Some(failed) = at("amdvi-selftest: FAIL") {
        return Err(format!("{}\nserial:\n{serial}", lines[failed]));
    }
    let stop = lines
        .iter()
        .position(|l| halted(l))
        .ok_or_else(|| format!("the machine never reached the halt path's line past the stop\nserial:\n{serial}"))?;
    at(AMDVI_TRANSLATING).ok_or_else(|| format!("the unit was never programmed\nserial:\n{serial}"))?;
    let mut said = Vec::new();
    for (step, verdict) in
        [("mapped to", "landed there"), ("on the identity domain", "landed there"), ("which the identity domain does not map", "refused")]
    {
        let found = at(step).map(|i| lines[i]).ok_or_else(|| format!("the selftest never said {step:?}\nserial:\n{serial}"))?;
        eprintln!("  [amdvi] {found}");
        if !found.ends_with(verdict) {
            return Err(format!("{found}\nserial:\n{serial}"));
        }
        said.push(found);
    }
    // `<function>'s write at <address>, which the identity domain …`. The
    // record is held to its function alone: QEMU 11.1.1 writes it with no
    // address and its flags off 48882's positions
    // (`issues/qemus-amd-iommu-page-fault-record-carries-no-address.md`).
    let function = said[2]
        .split_once("amdvi-selftest: ")
        .and_then(|(_, rest)| rest.split_once("'s write at "))
        .map(|(function, _)| function)
        .ok_or_else(|| format!("{:?} names no function", said[2]))?;
    let record = format!("iommu: DMA FAULT owner=kernel unit0 stream={function} ");
    let faults: Vec<usize> = (0..lines.len()).filter(|&i| lines[i].contains("iommu: DMA FAULT owner=")).collect();
    match faults.as_slice() {
        [one] if lines[*one].contains(&record) && lines[*one].ends_with("IO_PAGE_FAULT") && *one < stop => {
            eprintln!("  [amdvi] {}", lines[*one]);
        }
        _ => return Err(format!("not one record {record:?} … IO_PAGE_FAULT before the stop\nserial:\n{serial}")),
    }
    if let Some(death) = lines.iter().find(|l| super::serial::died(l).is_some() && !l.contains("owner=kernel")) {
        return Err(format!("the machine died of something other than the record: {death}\nserial:\n{serial}"));
    }
    Ok(())
}

/// What the supervisor says refusing netcase's NIC to its driver, on a machine
/// whose virtio NIC was created without `iommu_platform=on`.
fn untranslated_nic() -> String {
    format!(
        "supervisor: virtio-net was not started on PCI {CLAIMED_AT}: pci:02:00:00/1af4:1041 is a virtio function that \
         offers no VIRTIO_F_ACCESS_PLATFORM, so no unit translates its DMA (virtio 1.2 §6.1): refused"
    )
}

/// A virtio NIC that bypasses the unit ([`Profile::HeadlessNicUntranslated`])
/// is never handed to its driver: the supervisor reads its offer through the
/// claim, refuses it by name and starts no NIC driver, and netstack serves
/// with no link, saying so once its bound on a first lease is out. Only a
/// QEMU machine can make one: every bench function is behind its unit.
pub fn a_virtio_function_without_access_platform_is_refused() -> Result<(), String> {
    let options = BootOptions { profile: Profile::HeadlessNicUntranslated, ..Default::default() };
    let mut qemu = QemuInstance::boot_with_options(&netcase(), &[], &[], options);
    let refused = untranslated_nic();
    let gone = "netstack: DHCP: no lease as toyos-t14 on any link in";
    let mut text = qemu.boot_log().to_string();
    qemu::await_guest(&mut qemu, &mut text, "the supervisor's refusal", |t| t.contains(&refused)).map_err(|e| format!("{e}\n{text}"))?;
    // A quiet guest, for netstack's bound on its first lease: a ceiling past it.
    if !text.contains(gone) {
        text.push_str(&qemu.drain_until(std::time::Duration::from_secs(90), |l| l.contains(gone)));
    }
    if !text.contains(gone) {
        return Err(format!("netstack did not say a machine whose NIC has no driver has no address\n{text}"));
    }
    let log = Serial::named("boot console", text);
    log.must_be_clean()?;
    log.must_not_say("virtio-net: VirtIO: PCI")?;
    eprintln!("  [iommu] {refused}");
    Ok(())
}

/// The kernel's virtio console created without `iommu_platform=on`
/// ([`Profile::HeadlessConsoleUntranslated`]) bypasses the unit, so the kernel
/// tells it physical addresses and the boot reaches its ready marker over it.
/// Given a domain's addresses instead, it reads and writes whatever memory
/// those are, and the boot's console is lost. q35 only: no bench has a virtio
/// function.
pub fn the_kernel_tells_a_bypassing_console_physical_addresses() -> Result<(), String> {
    let options = BootOptions { profile: Profile::HeadlessConsoleUntranslated, ..Default::default() };
    let qemu = QemuInstance::boot_with_options(&netcase(), &[], &[], options);
    let log = Serial::named("boot console", qemu.boot_log().to_string());
    log.must_be_clean()?;
    log.must_say("offers no ACCESS_PLATFORM, so it is told physical addresses")?;
    log.must_say("Boot: complete")?;
    eprintln!("  [iommu] the console bypassing the unit was told physical addresses and carried the boot");
    Ok(())
}
