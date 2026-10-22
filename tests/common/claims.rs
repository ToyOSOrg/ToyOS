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

/// The kernel's record of a declaration it refused.
pub const REFUSED_DECLARATION: &str = "pcidev: the root bridges' memory is refused as declared";

/// What the `acpi` claim's holder declared the root bridges decode.
pub const DECLARED: &str = "pcidev: the acpi claim declared root bridge memory: ";

fn hex(text: &str) -> Option<u64> {
    u64::from_str_radix(text.trim().strip_prefix("0x")?, 16).ok()
}

/// `a..b` in hex, as the kernel's records write a span.
fn span(text: &str) -> Option<(u64, u64)> {
    let (from, to) = text.split_once("..")?;
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

/// Every address a domain mapped lies outside every root-bridge window the
/// `acpi` claim's holder declared: a bridge may route a request in one before
/// the unit sees it.
pub fn clear_of_host_bridges(log: &Serial) -> Result<(), String> {
    let windows = declared(log)?;
    let mut mapped = 0;
    // `iommu: domainN maps <phys>..<end> at <device address>[ again]`.
    for record in records(log).filter(|m| m.starts_with("iommu: domain") && m.contains(" maps ")) {
        let parsed = record.split_once(" maps ").and_then(|(_, rest)| {
            let (phys, at) = rest.split_once(" at ")?;
            let (from, to) = span(phys)?;
            let at = hex(at.trim_end_matches(" again"))?;
            Some((at, at + (to - from)))
        });
        let (at, end) = parsed.ok_or_else(|| format!("an unreadable mapping record: {record:?}"))?;
        if let Some((base, top)) = windows.iter().find(|(base, top)| *top > at && *base < end) {
            return Err(format!("{record:?} maps device addresses inside the root-bridge window {base:#x}..{top:#x}"));
        }
        mapped += 1;
    }
    if mapped == 0 {
        return Err(format!("no domain mapped anything on this boot:\n{}", log.text()));
    }
    eprintln!("  [claims] {mapped} mapping(s), none reaching a root-bridge window");
    Ok(())
}

/// The claim of the function at `at` refused for `why`, before anything on
/// it changed: no message armed and no entry written.
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
