//! `iommu_virtio_platform`: whether each virtio function QEMU creates behind
//! its emulated VT-d unit, and none created without one, negotiated
//! `VIRTIO_F_ACCESS_PLATFORM`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::qemu::{self, BootOptions, Profile, QemuInstance};
use super::serial::Serial;

/// The function `tests/netcase`'s netstack claims, as its `devices` row spells it.
const NETSTACK_CLAIMS: &str = "1af4:1041";

/// fileserver's word for DATA's directories served from memory.
const IN_MEMORY: &str = "are in memory and will not survive a reboot";

/// The boot config that runs `netstack` with a virtio NIC in front of it.
fn netcase() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/netcase")
}

/// Whether this machine's virtio functions are behind the unit at all.
///
/// QEMU keeps a virtio function on `&address_space_memory` — the unit bypassed,
/// whatever the tables say — unless it is created with `iommu_platform=on`
/// (`hw/virtio/virtio-bus.c:86-99`, `hw/virtio/virtio-pci.c:1400-1405` at
/// v11.1.1), and under identity mapping the two are indistinguishable. So the
/// argv says which functions were created behind a unit and the console says
/// which negotiated `VIRTIO_F_ACCESS_PLATFORM`. On [`Profile::HeadlessNoIommu`]
/// the guest reports `n` because QEMU never *offers* the bit
/// (`hw/virtio/virtio-bus.c:87-94`), not because the driver declined — it offers
/// blindly; the independence comes from [`declining_is_not_free`].
pub fn iommu_virtio_platform(test_config: &Path) -> Result<(), String> {
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
        // that runs it. A daemon's line is waited for on the stream: the boot
        // log ends at test-runner's `===READY===`, its first act, and nothing
        // orders that against what the programs the supervisor started before it print.
        // And the kernel's records of a claim with them: those reach the
        // console by the kernel's own road, which nothing orders against a
        // program's line either.
        let mut qemu = QemuInstance::boot_with_options(&netcase(), &[], &[], options);
        let said: Vec<String> = if behind_unit {
            vec![CLAIM_BOUNDED.to_string(), NETSTACK_NEGOTIATED.to_string(), bar_moved(), msix_armed()]
        } else {
            vec![
                DISKSERVER_REFUSED.to_string(),
                FILESERVER_WITHOUT_DATA.to_string(),
                not_handed_over(CLAIMED_AT, NOT_REMAPPED),
                not_handed_over(NVME_AT, NOT_REMAPPED),
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
        log.must_say("supervisor: started netstack")?;
        // The CPU's own source in the generator's key, which firmware's seed
        // alone would key without it wherever firmware answers EFI_RNG_PROTOCOL.
        log.must_say("random: RDRAND is mixed into the generator's key")?;

        // **Whether the NIC's function is handed to a process at all is the
        // machine's answer, not a choice.** A process driving a device writes
        // addresses into descriptors, so the substrate refuses a claim on a
        // function this machine cannot give an address space of its own — and
        // on a machine with no unit that is every function. So the arm with a
        // unit has three negotiators, one of them across the boundary, and the
        // arm without one has two and a refusal.
        let expected = if behind_unit {
            // And the claim netstack was given is bounded to its own function's
            // configuration space, which is what makes its capability walk —
            // an index by numbers the *device* wrote — safe to run at all.
            // netstack asks the kernel for a read past the end, one straddling it
            // and one misaligned, and refuses to drive a claim that answers any
            // of them.
            log.must_say(CLAIM_BOUNDED)?;
            // The two things a hand-over spends, on the same function and the
            // same machine the arm below requires to be unspent. Without this
            // pair those `must_not_say`s would pass against a kernel that had
            // stopped writing either line.
            log.must_say(&bar_moved())?;
            log.must_say(&msix_armed())?;
            created.len()
        } else {
            no_unit_is_no_claim(&log)?;
            created.len() - 1
        };

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
        if negotiated.len() != expected {
            return Err(format!(
                "{name}: QEMU created {} virtio function(s), {expected} of them for a driver \
                 this machine can bring up, and the guest negotiated features with {} — \
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
        let sound = class_function(&log, "0401")
            .ok_or_else(|| format!("{name}: this machine enumerated no audio function"))?;
        if !negotiated.iter().any(|(who, _)| *who == sound) {
            return Err(format!(
                "{name}: the audio function {sound} negotiated nothing, so whether it is behind \
                 the unit was never asked: {negotiated:?}"
            ));
        }
        eprintln!(
            "  [iommu] {name}: {} virtio function(s) behind a unit = {behind_unit}, the audio \
             function {sound} among them{}",
            negotiated.len(),
            if behind_unit { "" } else { "; the NIC's claim refused for want of a unit" }
        );
    }
    declining_is_not_free(test_config)
}

/// netstack's, once its claim answers nothing outside its own function.
const CLAIM_BOUNDED: &str =
    "netstack: this claim answers 4096 bytes of configuration space and refuses every access \
     outside them";
/// netstack's feature line, the kernel's shape under netstack's name.
const NETSTACK_NEGOTIATED: &str = "netstack: VirtIO: PCI ";
const DISKSERVER_REFUSED: &str =
    "diskserver: NOT SERVING — pci:1b36:0010 is on this machine and the kernel refused this service its claim";
const FILESERVER_WITHOUT_DATA: &str =
    "fileserver: the block service would not list its partitions (Refused(ClaimRefused)); DATA is absent this boot";

/// The slot QEMU's `-device` order puts `tests/netcase`'s NVMe controller on,
/// the one its diskserver row claims.
const NVME_AT: &str = "00:02.0";

/// Why a machine with no unit hands no function over, in the kernel's words.
const NOT_REMAPPED: &str = "its interrupts would not be remapped on this machine";

/// The kernel's record of a claim on the function at `at` refused for `why`.
fn not_handed_over(at: &str, why: &str) -> String {
    format!("pcidev: PCI {at} NOT HANDED OVER — {why}")
}

/// **A machine with no unit hands no function to a process**, and says so
/// three times over.
///
/// The ordering ruling this whole stage stands on
/// (`issues/every-driver-is-still-in-the-kernel.md`) is that moving a
/// driver out without the unit costs security: a message nothing remaps raises
/// any vector on any CPU, and a descriptor holding a physical address is an
/// arbitrary read and write over all of memory. So the kernel
/// refuses the claim by name, the supervisor says which device it could not
/// mint, and netstack exits rather than driving anything — and the machine
/// finishes booting, which is the half a refusal that panicked would fail. The
/// NVMe controller is refused the same, and DATA with it by name: a disk that
/// is there and cannot be used is never answered with memory.
fn no_unit_is_no_claim(log: &Serial) -> Result<(), String> {
    // netstack's own exit is the third saying, and is not read here.
    refused_claim(log, NETSTACK_CLAIMS, NOT_REMAPPED, &[NVME_AT])?;
    log.must_say(&not_handed_over(NVME_AT, NOT_REMAPPED))?;
    log.must_say("supervisor: diskserver: pci:1b36:0010 is on this machine and could not be handed over")?;
    log.must_say(DISKSERVER_REFUSED)?;
    log.must_say(FILESERVER_WITHOUT_DATA)?;
    log.must_not_say(IN_MEMORY)?;
    // And this machine handed *nothing* over, which is more than the claim's
    // own refusal says: with no unit there is no function any process could be
    // given an address space for.
    log.must_not_say("handed over on slot")?;
    Ok(())
}

/// The control that makes the two arms above mean something: a guest that
/// declines the feature its host offered gets no device, not a bypassing one.
/// `virtio_validate_features` returns `-EFAULT` and `virtio_set_status` returns
/// before it stores the status (`hw/virtio/virtio.c:2270-2276` and `:2292-2299`
/// at v11.1.1), so `FEATURES_OK` never sticks. The actuator withholds the bit
/// from every virtio device but the console, and each of them is refused for it.
fn declining_is_not_free(test_config: &Path) -> Result<(), String> {
    let qemu = QemuInstance::boot_with_options(
        test_config,
        &[],
        &[],
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
    refused.iter().for_each(|line| eprintln!("  [iommu] declined: {line}"));
    Ok(())
}

/// The slot QEMU's `-device` order puts the function netstack claims on, and the
/// address every judge below is an assertion about.
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
fn bar_moved() -> String {
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
/// nothing**: no BAR of that function moved, neither of its two message
/// mechanisms is armed, `claims` reached no holder, and the supervisor said so in the
/// boot config's own spelling. `beside` is every other function this machine
/// refuses, each judged by its own caller.
///
/// `slot_space` put back below `place_bars` reds on the two unspent lines.
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
    log.must_not_say(&bar_moved())?;
    // All the way out to userland, rather than a kernel that logged a refusal
    // and handed netstack a NIC anyway. The supervisor names what it could not mint in the
    // config's own spelling, and **with this refusal's own word**: the machine
    // has the function, so "no such device on this machine" would be false.
    log.must_say(&format!(
        "supervisor: netstack: pci:{claims} is on this machine and could not be handed over"
    ))?;
    Ok(())
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
