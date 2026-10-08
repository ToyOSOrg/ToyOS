//! A claimed function at the unit, on the T14: the remapping entry each claim
//! of it writes and each release takes back, and the window its domain hands
//! addresses out of. Every judge reads the kernel's own records.

use toyos_build::bootlog;

use super::serial::Serial;

/// The T14's I219, as the kernel's hand-over record spells its id.
pub const I219: &str = "8086:15fc";

/// The job that claims the T14's I219, gives it back and claims it again.
pub const RECLAIM: &str = "test_rs_pci_reclaim";

/// The kernel's reason on a claim's refusal line where the units do not remap.
const NOT_REMAPPED: &str = "its interrupts would not be remapped on this machine";

/// The kernel's line where firmware's DMAR flags leave interrupt remapping off,
/// which `iommu-no-remap` stands in for: the premise of the refusal.
const INTR_REMAP_CLEAR: &str = "leave INTR_REMAP clear, so firmware says this platform does not remap";

fn records(log: &Serial) -> impl Iterator<Item = &str> {
    log.text().lines().filter_map(bootlog::message)
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
    let mut written = Vec::new();
    let mut cleared = Vec::new();
    for record in records(log) {
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
        let needle = format!("[{I219}] {what} slot ");
        records(log).filter(|m| m.starts_with("pcidev: PCI ") && m.contains(&needle)).count()
    };
    let (handed, released) = (count("handed over on"), count("released from"));
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

/// Every domain's addresses lie outside every root-bridge window firmware
/// declared: a bridge may route a request in one of those before the unit
/// sees it.
pub fn clear_of_host_bridges(log: &Serial) -> Result<(), String> {
    const DECLARED: &str = "pcidev: firmware declared root bridge memory: ";
    let declared = records(log)
        .find_map(|m| m.strip_prefix(DECLARED))
        .ok_or_else(|| format!("no `{DECLARED}` record:\n{}", log.text()))?;
    let range = |text: &str| -> Option<(u64, u64)> {
        let (from, to) = text.split_once("..")?;
        let hex = |t: &str| u64::from_str_radix(t.trim().strip_prefix("0x")?, 16).ok();
        Some((hex(from)?, hex(to)?))
    };
    let windows = declared
        .split(", ")
        .map(|w| w.strip_prefix("mem ").and_then(range).ok_or_else(|| format!("{w:?} in {declared:?}")))
        .collect::<Result<Vec<_>, _>>()?;
    let mut domains = 0;
    for record in records(log).filter(|m| m.starts_with("iommu: domain")) {
        let Some((_, span)) = record.split_once(" addresses from ") else { continue };
        let (floor, ceiling) = span
            .split_once(" to ")
            .and_then(|(from, to)| range(&format!("{from}..{to}")))
            .ok_or_else(|| format!("an unreadable domain record: {record:?}"))?;
        if let Some((base, end)) = windows.iter().find(|(base, end)| *end > floor && *base < ceiling) {
            return Err(format!(
                "{record:?} hands out addresses inside the root-bridge window {base:#x}..{end:#x}"
            ));
        }
        domains += 1;
    }
    if domains == 0 {
        return Err(format!("no domain was made on this boot:\n{}", log.text()));
    }
    eprintln!("  [claims] {domains} domain(s), none reaching a root-bridge window");
    Ok(())
}

/// On a machine whose units do not remap, the I219's claim is refused by that
/// reason before anything on the function is armed, and the boot completes.
pub fn refused_unremapped(log: &Serial) -> Result<(), String> {
    log.must_say(INTR_REMAP_CLEAR)?;
    let at = function(log)?;
    log.must_say(&format!("pcidev: PCI {at} NOT HANDED OVER — {NOT_REMAPPED}"))?;
    log.must_not_say(&format!("[{I219}] handed over"))?;
    log.must_not_say(&format!("PCI {at}: msi address="))?;
    log.must_not_say(&format!("PCI {at}: msix address="))?;
    log.must_not_say(&format!(" source={at} "))?;
    log.must_not_say(&format!("pcidev: PCI {at} BAR"))?;
    log.must_say(bootlog::COMPLETE)?;
    Ok(())
}
