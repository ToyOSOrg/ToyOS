//! Measurement scout, never landed: the Wi-Fi design's S1 readings, read off
//! one boot's whole text, the kernel's records and the programs' lines, the
//! same on the T14's log and on a QEMU console.
//!
//! **The A/B** ([`Compat`]): the controller `xhci-leave=` leaves to a claim,
//! its message written by the kernel in compatibility format on boot A
//! (`claim-compat-format`) and remapped on boot B. What the unit did with it
//! is two records: a fault naming the function, and the slot's first message.
//!
//! **The read-out** ([`readout`]): the kernel's `pci-readout:` lines, the
//! root bridges' memory and the DMAR's units and reserved regions as every
//! boot logs them, and acpiserver's `acpi-readout:` lines, put together for
//! the radio: the unit and the regions whose scope reaches it, and the names
//! under its ACPI device.

use std::collections::{BTreeMap, BTreeSet};

/// VT-d 4.0's fault reason for a compatibility-format message blocked
/// (§5.1.4), as `kernel/src/arch/x86_64/vtd/fault.rs` writes it.
const BLOCKED: &str = "reason=0x25";

/// Interrupt Format, bit 4 of the address (VT-d 4.0 §5.1.5.2): set names a
/// remapping entry, clear is compatibility format.
const REMAPPABLE: u32 = 1 << 4;

/// The text after `needle` on each line that carries it.
fn after<'a>(text: &'a str, needle: &'a str) -> impl Iterator<Item = &'a str> + 'a {
    text.lines().filter_map(move |line| line.split_once(needle).map(|(_, rest)| rest))
}

/// Each line carrying `needle`, from `needle` on.
fn from<'a>(text: &'a str, needle: &'a str) -> impl Iterator<Item = String> + 'a {
    after(text, needle).map(move |rest| format!("{needle}{rest}"))
}

fn hex(word: &str) -> Result<u32, String> {
    u32::from_str_radix(word.trim_start_matches("0x"), 16).map_err(|e| format!("{word:?}: {e}"))
}

/// What one boot of the A/B says about the claim's message.
pub struct Compat {
    pub at: String,
    pub slot: String,
    pub address: u32,
    pub data: u32,
    /// The kernel's word that it wrote the message in compatibility format.
    pub actuated: bool,
    /// Every fault record naming the function.
    pub faults: Vec<String>,
    /// The slot's first message reached its handler.
    pub delivered: bool,
    /// Every unit's state once it translates: `IRE`, `CFIS` and `EIME` among it.
    pub units: Vec<String>,
    /// What the job said, usbd's lines among it.
    pub job: Vec<String>,
}

impl Compat {
    pub fn read(text: &str) -> Result<Self, String> {
        let left = after(text, "xHCI: leaving PCI ")
            .next()
            .ok_or("the kernel never said it left a controller to a claim (xhci-leave)")?;
        let mut words = left.split_whitespace();
        let (at, id) = (words.next().unwrap_or_default().to_string(), words.next().unwrap_or_default());
        let handed = format!("pcidev: PCI {at} [{id}] handed over on slot ");
        let slot = after(text, &handed)
            .next()
            .and_then(|rest| rest.split(',').next())
            .ok_or_else(|| format!("{at} was never handed over to a claim"))?
            .to_string();
        let msi = format!("PCI {at}: msi address=");
        let msix = format!("PCI {at}: msix address=");
        let message = after(text, &msi)
            .chain(after(text, &msix))
            .next()
            .ok_or_else(|| format!("{at}'s claim was armed on no message"))?;
        let (address, data) = message
            .split_once(" data=")
            .ok_or_else(|| format!("{at}'s message line reads {message:?}"))?;
        let first = format!("pcidev: slot {slot} took its first message on");
        let stream = format!("stream={at} ");
        Ok(Compat {
            slot,
            address: hex(address.trim())?,
            data: hex(data.split_whitespace().next().unwrap_or_default())?,
            actuated: after(text, "(claim-compat-format)").next().is_some(),
            faults: from(text, "iommu: DMA FAULT ").filter(|f| f.contains(&stream)).collect(),
            delivered: text.contains(&first),
            units: from(text, "iommu: unit").filter(|u| u.contains(" translating gsts=")).collect(),
            job: from(text, "scout_compat_format: ").collect(),
            at,
        })
    }

    pub fn compatibility_format(&self) -> bool {
        self.address & REMAPPABLE == 0
    }

    pub fn blocked(&self) -> bool {
        self.faults.iter().any(|f| f.contains(BLOCKED))
    }

    pub fn summary(&self) -> String {
        let mut out = format!(
            "{} on slot {}: address={:#010x} data={:#010x} ({}), {}, {} fault record(s) naming it",
            self.at,
            self.slot,
            self.address,
            self.data,
            if self.compatibility_format() { "compatibility format" } else { "remappable format" },
            if self.delivered { "delivered" } else { "not delivered" },
            self.faults.len(),
        );
        for line in self.faults.iter().chain(&self.units).chain(&self.job) {
            out.push_str("\n      ");
            out.push_str(line);
        }
        out
    }

    /// Both boots' premises: A's message is the actuator's compatibility
    /// format and B's the remapped one, and B, the control, was delivered
    /// with no fault.
    fn premises(a: &Self, b: &Self) -> Result<(), String> {
        if !a.actuated || !a.compatibility_format() {
            return Err(format!("A's claim was not armed in compatibility format by the actuator: {}", a.summary()));
        }
        if b.actuated || b.compatibility_format() {
            return Err(format!("B's claim was not armed in remappable format: {}", b.summary()));
        }
        if !b.delivered || !b.faults.is_empty() {
            return Err(format!("B, the control, was not delivered without a fault: {}", b.summary()));
        }
        Ok(())
    }
}

/// The T14's verdict, fixed before the run: A blocked, by fault 0x25 and no
/// delivery; B delivered.
pub fn compat_on_metal(a: &str, b: &str) -> Result<(), String> {
    let (a, b) = (Compat::read(a)?, Compat::read(b)?);
    eprintln!("  [s1] A: {}", a.summary());
    eprintln!("  [s1] B: {}", b.summary());
    Compat::premises(&a, &b)?;
    if a.delivered {
        return Err(format!(
            "A's compatibility-format message was delivered under IRE: a kernel-wide isolation \
             defect, and P1 waits (the owner's ruling, Q2): {}",
            a.summary()
        ));
    }
    if !a.blocked() {
        return Err(format!("A was not delivered and the unit recorded no {BLOCKED}: {}", a.summary()));
    }
    eprintln!("  [s1] compatibility format blocked: fault 0x25 and no delivery in A, delivery in B");
    Ok(())
}

/// QEMU's reading: it passes compatibility format through (QEMU 11.1
/// `hw/i386/intel_iommu.c`, `issues/qemu-passes-compatibility-format-interrupts.md`),
/// so A is delivered with no fault, as B is.
pub fn compat_on_qemu(a: &str, b: &str) -> Result<(), String> {
    let (a, b) = (Compat::read(a)?, Compat::read(b)?);
    eprintln!("  [s1] QEMU A: {}", a.summary());
    eprintln!("  [s1] QEMU B: {}", b.summary());
    Compat::premises(&a, &b)?;
    if !a.delivered || !a.faults.is_empty() {
        return Err(format!("QEMU A was not delivered without a fault, which QEMU 11.1 does: {}", a.summary()));
    }
    eprintln!("  [s1] QEMU delivers A's compatibility-format message, as it delivers B's");
    Ok(())
}

/// One `key=value` word of a line.
fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split_whitespace().find_map(|word| word.strip_prefix(key)?.strip_prefix('='))
}

/// A `bb:dd.f` name's device and function, as `_ADR` packs them (ACPI 6.5 §6.1.1).
fn adr(at: &str) -> Option<u64> {
    let (_, devfn) = at.split_once(':')?;
    let (dev, func) = devfn.split_once('.')?;
    Some(u64::from_str_radix(dev, 16).ok()? << 16 | func.parse::<u64>().ok()?)
}

/// The read-out, put together and printed; `Err` where a part of it is missing.
/// `radio` asks that the machine have one.
pub fn readout(text: &str, radio: bool) -> Result<(), String> {
    let kernel: Vec<&str> = after(text, "pci-readout: ").collect();
    if kernel.is_empty() {
        return Err("no pci-readout line: the kernel was not armed with `pci-readout`".to_string());
    }
    let mut functions: BTreeMap<String, (String, Vec<&str>)> = BTreeMap::new();
    for line in &kernel {
        let Some(rest) = line.strip_prefix("PCI ") else { continue };
        let Some((at, what)) = rest.split_once(' ') else { continue };
        let entry = functions.entry(at.to_string()).or_default();
        if what.starts_with('[') {
            entry.0 = what.to_string();
        }
        entry.1.push(what);
    }
    for line in kernel.iter().filter(|l| !l.starts_with("PCI ")) {
        eprintln!("  [s1] {line}");
    }
    for root in from(text, "pcidev: firmware declared root bridge memory:") {
        eprintln!("  [s1] {root}");
    }
    let radios: Vec<&String> = functions.iter().filter(|(_, (head, _))| head.contains("] radio ")).map(|(at, _)| at).collect();
    if radio && radios.is_empty() {
        return Err("no radio (class 02:80) on this machine's read-out".to_string());
    }
    for (at, (_, lines)) in &functions {
        eprintln!("  [s1] PCI {at}:");
        for line in lines {
            eprintln!("  [s1]     {line}");
        }
    }
    for at in radios {
        dmar(text, at, &functions[at].1);
        acpi(text, at, &functions[at].1)?;
    }
    acpi_summary(text)
}

/// The bridges on `lines`'s path, nearest first.
fn bridges<'a>(lines: &[&'a str]) -> Vec<&'a str> {
    lines.iter().filter_map(|l| l.strip_prefix("behind PCI ")?.split_whitespace().next()).collect()
}

/// Which unit's scope reaches `at` and which reserved regions name it, by the
/// DMAR's own lines: an endpoint scope naming it, a bridge scope naming a
/// bridge on its path (VT-d 4.0 §8.3.1), else the unit that includes every
/// function no scope names.
fn dmar(text: &str, at: &str, lines: &[&str]) {
    let path = bridges(lines);
    let reaches = |scope: &str| {
        let mut words = scope.split_whitespace();
        match (words.next(), words.next()) {
            (Some("pci-endpoint"), Some(named)) => named == at,
            (Some("pci-bridge"), Some(named)) => path.contains(&named),
            _ => false,
        }
    };
    let mut named_by: BTreeSet<String> = BTreeSet::new();
    let mut catch_all: Vec<String> = Vec::new();
    let mut regions: Vec<String> = Vec::new();
    for line in from(text, "iommu: ") {
        let Some(rest) = line.strip_prefix("iommu: ") else { continue };
        let Some((owner, said)) = rest.split_once(' ') else { continue };
        if let Some(scope) = said.strip_prefix("scope ") {
            if scope.contains("requester id needs a bridge walk") {
                eprintln!("  [s1] DMAR: {owner}'s scope is a path this read-out does not walk: {scope}");
            } else if reaches(scope) {
                named_by.insert(owner.to_string());
            }
        } else if owner.starts_with("unit") && field(said, "pci_all") == Some("y") && said.starts_with('@') {
            catch_all.push(owner.to_string());
        } else if owner.starts_with("rmrr") && said.starts_with("seg=") {
            regions.push(line.clone());
        }
    }
    let units: Vec<&String> = named_by.iter().filter(|o| o.starts_with("unit")).collect();
    let rmrrs: Vec<&String> = named_by.iter().filter(|o| o.starts_with("rmrr")).collect();
    if units.is_empty() {
        eprintln!("  [s1] DMAR: no unit's scope names {at} or a bridge above it; it is the INCLUDE_PCI_ALL unit's: {catch_all:?}");
    } else {
        eprintln!("  [s1] DMAR: {at} is in the scope of {units:?}");
    }
    eprintln!("  [s1] DMAR: reserved regions on this machine: {regions:?}; naming {at} or a bridge above it: {rmrrs:?}");
}

/// The radio's ACPI device: under a host bridge, the device whose `_ADR` is
/// the root port's on the root bus, and under it the one whose `_ADR` is the
/// radio's. Every name below it is printed.
fn acpi(text: &str, at: &str, lines: &[&str]) -> Result<(), String> {
    let readout: Vec<&str> = after(text, "acpi-readout: ").collect();
    let host_bridges: Vec<&str> = readout.iter().filter_map(|l| l.strip_prefix("host bridge ")).collect();
    let devices: Vec<(&str, &str)> = readout
        .iter()
        .filter_map(|l| l.strip_prefix("device ")?.split_once(" _ADR="))
        .collect();
    let parent = |path: &str| path.rsplit_once('.').map(|(above, _)| above.to_string());
    let Some(root_port) = bridges(lines).last().copied() else {
        eprintln!("  [s1] ACPI: {at} sits on a root bus, so no root port leads to it");
        return Ok(());
    };
    let (Some(port_adr), Some(radio_adr)) = (adr(root_port), adr(at)) else {
        return Err(format!("{root_port} or {at} is no bb:dd.f name"));
    };
    let ports: Vec<&str> = devices
        .iter()
        .filter(|(path, value)| {
            hex(value).ok().map(u64::from) == Some(port_adr)
                && parent(path).is_some_and(|p| host_bridges.contains(&p.as_str()))
        })
        .map(|(path, _)| *path)
        .collect();
    eprintln!("  [s1] ACPI: host bridge(s) {host_bridges:?}; root port {root_port} (_ADR={port_adr:#010x}) is {ports:?}");
    let mine: Vec<&str> = devices
        .iter()
        .filter(|(path, value)| {
            hex(value).ok().map(u64::from) == Some(radio_adr) && parent(path).is_some_and(|p| ports.contains(&p.as_str()))
        })
        .map(|(path, _)| *path)
        .collect();
    let [device] = mine.as_slice() else {
        eprintln!("  [s1] ACPI: {} device(s) under {ports:?} carry _ADR={radio_adr:#010x}: {mine:?}", mine.len());
        return Ok(());
    };
    let below = format!("{device}.");
    let names: Vec<&str> = readout.iter().filter_map(|l| l.strip_prefix("name ")).filter(|n| n.starts_with(&below)).collect();
    eprintln!("  [s1] ACPI: {at} is {device}, with {} name(s) below it:", names.len());
    for name in names {
        eprintln!("  [s1]     {name}");
    }
    Ok(())
}

/// Every host bridge and device with an `_ADR` the server read out, and its summary.
fn acpi_summary(text: &str) -> Result<(), String> {
    for line in after(text, "acpi-readout: ").filter(|l| l.starts_with("host bridge ") || l.starts_with("device ")) {
        eprintln!("  [s1] ACPI: {line}");
    }
    let summary = from(text, "acpiserver: acpi-readout: ")
        .next()
        .ok_or("acpiserver said no acpi-readout summary: it was not given --pci-readout, or kept no namespace")?;
    eprintln!("  [s1] {summary}");
    Ok(())
}
