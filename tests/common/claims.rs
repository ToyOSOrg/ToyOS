//! A claimed function at the unit, on the T14: the remapping entry each claim
//! of it writes and each release takes back, and the window its domain hands
//! addresses out of. Every judge reads the kernel's own records.

use toyos_build::bootlog;

use super::serial::Serial;

/// The T14's I219, as the kernel's hand-over record spells its id.
const I219: &str = "8086:15fc";

/// The job that claims the T14's I219, gives it back and claims it again.
pub const RECLAIM: &str = "test_rs_pci_reclaim";

/// The job that measures the I219's interrupts on its own claim.
pub const MSI: &str = "test_rs_i219_msi";

/// What that job prints each reading under.
const MSI_SAYS: &str = "i219_msi: ";

/// The kernel's reason on a claim's refusal line where the units do not remap.
pub const NOT_REMAPPED: &str = "its interrupts would not be remapped on this machine";

/// The kernel's reason where the units remap and one filters no SMI.
pub const SMI_UNFILTERED: &str = "an IOMMU unit on this machine filters no SMI";

/// The kernel's line where firmware's DMAR flags leave interrupt remapping off,
/// which `iommu-no-remap` stands in for: the premise of the refusal.
const INTR_REMAP_CLEAR: &str = "leave INTR_REMAP clear, so firmware says this platform does not remap";

fn records(log: &Serial) -> impl Iterator<Item = &str> {
    log.text().lines().filter_map(bootlog::message)
}

/// The kernel's records from `job`'s spawn to its exit: what it did, apart
/// from every other job on a shared boot. Both ends are the kernel's own
/// records, so nothing another channel carries can reorder them.
fn job_records<'a>(log: &'a Serial, job: &str) -> Result<Vec<&'a str>, String> {
    let spawned = format!("/{job} pid=");
    let mut all = records(log);
    let pid = all
        .by_ref()
        .find_map(|m| m.strip_prefix("spawn: ")?.split_once(&spawned)?.1.split_whitespace().next())
        .ok_or_else(|| format!("no spawn record names {job}:\n{}", log.text()))?;
    let exited = format!("{}{} pid={pid} ", bootlog::EXIT, bootlog::recorded_name(job));
    let mut window = Vec::new();
    for record in all {
        if record.starts_with(&exited) {
            return Ok(window);
        }
        window.push(record);
    }
    Err(format!("{job} pid={pid} never exited:\n{}", log.text()))
}

/// One `key=value` word of a record.
fn field<'a>(record: &'a str, key: &str) -> Option<&'a str> {
    record.split_whitespace().find_map(|word| word.strip_prefix(key)?.strip_prefix('='))
}

/// Where the I219 is, off its enumeration record, so no judge here names the
/// T14's address for it.
fn function(log: &Serial) -> Result<String, String> {
    let (vendor, device) = I219.split_once(':').expect("an id is vendor:device");
    let id = format!(" vendor={vendor} device={device} ");
    let mut found = records(log).filter(|m| m.contains(&id)).filter_map(|m| {
        m.trim_start().strip_prefix("PCI ")?.split_whitespace().next().map(str::to_string)
    });
    match (found.next(), found.next()) {
        (Some(at), None) => Ok(at),
        (None, _) => Err(format!("no enumeration record names {I219}:\n{}", log.text())),
        (Some(_), Some(_)) => Err(format!("two enumeration records name {I219}:\n{}", log.text())),
    }
}

/// Both claims of the I219 wrote one remapping entry, and each release left
/// that entry not present.
pub fn reuses_its_entry(log: &Serial) -> Result<(), String> {
    let at = function(log)?;
    let window = job_records(log, RECLAIM)?;
    let mut written = Vec::new();
    let mut cleared = Vec::new();
    for &record in &window {
        let Some(rest) = record.strip_prefix("iommu: irte") else { continue };
        if field(record, "source") != Some(at.as_str()) {
            continue;
        }
        let index = rest.split_whitespace().next().unwrap_or_default().to_string();
        let present = field(record, "p")
            .ok_or_else(|| format!("a remapping record with no p=: {record:?}"))?
            .to_string();
        if record.ends_with(" released") {
            cleared.push((index, present));
        } else {
            written.push(index);
        }
    }
    let count = |what: &str| {
        let needle = format!("[{I219}] {what}");
        window.iter().filter(|m| m.starts_with("pcidev: PCI ") && m.contains(&needle)).count()
    };
    let (handed, released) = (count("handed over"), count("released"));
    if (handed, released) != (2, 2) {
        return Err(format!(
            "{I219} was handed over {handed} time(s) and released {released}, want 2 and 2:\n{}",
            log.text()
        ));
    }
    match written.as_slice() {
        [first, second] if first == second => {}
        _ => {
            return Err(format!(
                "the two claims of {at} wrote remapping entries {written:?}, want one entry twice"
            ))
        }
    }
    let want = (written[0].clone(), "0".to_string());
    if cleared != [want.clone(), want] {
        return Err(format!(
            "the two releases of {at} left (entry, p) {cleared:?}, want irte{} at p=0 twice",
            written[0]
        ));
    }
    eprintln!("  [claims] {at}: irte{} written by both claims, not present after each release", written[0]);
    Ok(())
}

/// What the `acpi` claim's holder declared the root bridges decode.
pub const DECLARED: &str = "pcidev: the acpi claim declared root bridge memory: ";

/// `a..b` in hex, as the kernel's records write a span.
fn span(text: &str) -> Option<(u64, u64)> {
    let (from, to) = text.split_once("..")?;
    let hex = |t: &str| u64::from_str_radix(t.trim().strip_prefix("0x")?, 16).ok();
    Some((hex(from)?, hex(to)?))
}

/// The declared windows, as `(start, end)`.
fn declared(log: &Serial) -> Result<Vec<(u64, u64)>, String> {
    let declared = records(log)
        .find_map(|m| m.strip_prefix(DECLARED))
        .ok_or_else(|| format!("no `{DECLARED}` record:\n{}", log.text()))?;
    declared
        .split(", ")
        .map(|w| w.strip_prefix("mem ").and_then(span).ok_or_else(|| format!("{w:?} in {declared:?}")))
        .collect()
}

/// Each mapping domain `id`'s records say it made, as `(physical start,
/// physical end, device address)`.
pub fn mappings(log: &Serial, id: &str) -> Result<Vec<(u64, u64, u64)>, String> {
    let head = format!("iommu: domain{id} maps ");
    records(log)
        .filter_map(|m| m.strip_prefix(head.as_str()))
        .map(|rest| {
            let (phys, at) = rest.split_once(" at ").ok_or_else(|| format!("{rest:?}"))?;
            let (start, end) = span(phys).ok_or_else(|| format!("{rest:?}"))?;
            let at = u64::from_str_radix(at.split_whitespace().next().and_then(|a| a.strip_prefix("0x")).ok_or_else(|| format!("{rest:?}"))?, 16)
                .map_err(|e| format!("{rest:?}: {e}"))?;
            Ok((start, end, at))
        })
        .collect()
}

/// Whether domain `id`'s record says it maps each pool at identity: a kernel
/// driver's.
pub fn at_identity(log: &Serial, id: &str) -> bool {
    let head = format!("iommu: domain{id} ");
    records(log).any(|m| m.starts_with(head.as_str()) && m.contains(" maps each pool at identity"))
}

/// No domain reaches a root-bridge window the `acpi` claim's holder declared:
/// a bridge may route a request in one before the unit sees it. A claimed
/// function's domain hands out addresses outside every window; a kernel
/// driver's maps each pool at its own physical address, which lies outside
/// every window too.
pub fn clear_of_host_bridges(log: &Serial) -> Result<(), String> {
    let windows = declared(log)?;
    let inside = |(floor, ceiling): (u64, u64)| windows.iter().copied().find(|(base, end)| *end > floor && *base < ceiling);
    let (mut claimed, mut identity) = (0, 0);
    for record in records(log).filter(|m| m.starts_with("iommu: domain")) {
        let id = record["iommu: domain".len()..].split_whitespace().next().unwrap_or_default();
        if record.contains(" maps each pool at identity") {
            for (start, end, at) in mappings(log, id)? {
                if at != start {
                    return Err(format!("domain{id} is a kernel driver's and maps {start:#x}..{end:#x} at {at:#x}"));
                }
                if let Some((base, end)) = inside((start, end)) {
                    return Err(format!("domain{id} maps a pool inside the root-bridge window {base:#x}..{end:#x}"));
                }
            }
            identity += 1;
            continue;
        }
        let Some((_, addresses)) = record.split_once(" addresses from ") else { continue };
        let (floor, ceiling) = addresses
            .split_once(" to ")
            .and_then(|(from, to)| span(&format!("{from}..{to}")))
            .ok_or_else(|| format!("an unreadable domain record: {record:?}"))?;
        if let Some((base, end)) = inside((floor, ceiling)) {
            return Err(format!("{record:?} hands out addresses inside the root-bridge window {base:#x}..{end:#x}"));
        }
        claimed += 1;
    }
    if claimed + identity == 0 {
        return Err(format!("no domain was made on this boot:\n{}", log.text()));
    }
    eprintln!("  [claims] {claimed} claimed domain(s) and {identity} kernel driver's, none reaching a root-bridge window");
    Ok(())
}

/// On a machine whose units do not remap, the I219's claim is refused by that
/// reason before anything on the function is armed, and the boot completes.
pub fn refused_unremapped(log: &Serial) -> Result<(), String> {
    log.must_say(INTR_REMAP_CLEAR)?;
    let at = function(log)?;
    log.must_not_say(&format!("[{I219}] handed over"))?;
    refused_at(log, &at, NOT_REMAPPED)?;
    log.must_say(bootlog::COMPLETE)?;
    Ok(())
}

/// The claim of the function at `at` refused for `why`, before anything on
/// it changed: no message armed, no entry written, no BAR moved.
pub fn refused_at(log: &Serial, at: &str, why: &str) -> Result<(), String> {
    log.must_say(&format!("pcidev: PCI {at} NOT HANDED OVER — {why}"))?;
    log.must_not_say(&format!("PCI {at}: msi address="))?;
    log.must_not_say(&format!("PCI {at}: msix address="))?;
    log.must_not_say(&format!(" source={at} "))?;
    log.must_not_say(&format!("pcidev: PCI {at} BAR"))?;
    Ok(())
}

/// The job's claim of the I219 armed on MSI and not MSI-X, and the job's
/// readings — each judged by the job's own exit — read out.
pub fn messages_on_its_msi_vector(kernel: &Serial, log: &Serial) -> Result<(), String> {
    let at = function(kernel)?;
    let window = job_records(kernel, MSI)?;
    let says = |needle: &str| window.iter().any(|m| m.contains(needle));
    if !says(&format!("PCI {at}: msi address=")) || says(&format!("PCI {at}: msix address=")) {
        return Err(format!("{MSI}'s claim of {at} was not armed on MSI alone:\n{}", window.join("\n")));
    }
    let said: Vec<&str> = log.text().lines().filter_map(|l| Some(l.split_once(MSI_SAYS)?.1)).collect();
    if said.last() != Some(&"done") {
        return Err(format!("i219_msi never said it was done:\n{}", said.join("\n")));
    }
    for line in said {
        eprintln!("  [i219] {line}");
    }
    Ok(())
}
