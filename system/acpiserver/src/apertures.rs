//! The memory the PCI root bridges decode, declared to the kernel once
//! ([`Kernel::declare`]): every PCI claim on the machine waits for it, so it
//! is read as soon as the namespace is initialised (ACPI 6.5 §6.5.1).
//!
//! **A root is a PCI host bridge by its `_HID` or a `_CID`** (PNP0A03 or
//! PNP0A08, §6.1.5, §6.1.2) **below no other one**; a root whose `_STA` says
//! it is not present decodes nothing and is not read. Its windows are what
//! `toyos_acpi::memory_windows` reads of its `_CRS` (§6.2.2).
//!
//! **The declaration is every root's windows together, or none at all**: a
//! root whose `_STA` or `_CRS` is refused, a list the decoder refuses, more
//! windows than one declaration carries, and a namespace with no root are
//! each said by name and declared as no window, which the kernel refuses —
//! and with it every PCI claim, rather than one placed by a list with a hole
//! in it. A machine with no namespace declares the same.

use toyos_abi::acpi::{RootBridgeWindow, MAX_WINDOWS};
use toyos_aml::{Kind, Value};

use crate::aml::{kind, Aml};
use crate::host::{Declared, Kernel, Stopping, OWN};
use crate::init::{evaluate, host_bridge, Census, Names, PRESENT};

/// Every root bridge's windows, in walk order, or why there is no
/// declaration.
pub fn read<K: Kernel>(aml: &mut Aml<'_, K>, names: &Names, census: &mut Census) -> Result<Vec<RootBridgeWindow>, String> {
    let devices = names.objects.iter().filter(|named| named.kind == Kind::Device).map(|named| named.path.as_str());
    let mut bridges: Vec<&str> = Vec::new();
    for device in devices {
        if !bridges.iter().any(|above| below(device, above)) && host_bridge(aml, names, device, census) {
            bridges.push(device);
        }
    }
    if bridges.is_empty() {
        return Err("the namespace names no PCI host bridge".into());
    }
    let mut windows = Vec::new();
    for root in bridges {
        if !present(aml, names, root, census)? {
            println!("{OWN}{root} is not present, so it decodes nothing");
            continue;
        }
        let path = format!("{root}._CRS");
        let crs = match aml.interpreter.evaluate(&mut aml.host, &path, &[]) {
            Ok(Value::Buffer(crs)) => crs,
            Ok(_) => return Err(said(&path, "a root bridge's _CRS is no buffer")),
            Err(why) => return Err(said(&path, &format!("a root bridge's _CRS was refused: {}", kind(&why)))),
        };
        let mut out = [RootBridgeWindow::default(); MAX_WINDOWS];
        let room = MAX_WINDOWS - windows.len();
        match toyos_acpi::memory_windows(&crs[..], 0, &mut out[..room]) {
            Ok(count) => windows.extend_from_slice(&out[..count]),
            Err(why) => return Err(said(&path, &format!("a root bridge's _CRS is no list of windows: {why}"))),
        }
    }
    Ok(windows)
}

/// Declare what [`read`] answers, or no window where it refused, saying
/// which and what the kernel did with it.
pub fn declare<K: Kernel>(kernel: &K, read: Result<Vec<RootBridgeWindow>, String>) -> Result<(), Stopping> {
    let windows = match read {
        Ok(windows) => windows,
        Err(why) => {
            println!("acpiserver: no window declared, so every PCI claim is refused: {why}");
            Vec::new()
        }
    };
    let listed: Vec<String> = windows.iter().map(|w| format!("mem {:#x}..{:#x}", w.base, w.end())).collect();
    match kernel.declare(&windows)? {
        Declared::Held => println!("acpiserver: the root bridges' memory declared: {}", listed.join(", ")),
        Declared::Refused if windows.is_empty() => {}
        Declared::Refused => println!("acpiserver: the kernel refused the root bridges' memory as declared ({}); its log says why", listed.join(", ")),
        Declared::Late => println!("acpiserver: the kernel already holds an answer for the root bridges' memory, so nothing is declared"),
    }
    Ok(())
}

/// Whether `device` lies in `above`'s scope.
fn below(device: &str, above: &str) -> bool {
    device.strip_prefix(above).is_some_and(|rest| rest.starts_with('.'))
}

/// Whether `root` is present: bit 0 of its `_STA`, where it has one; a
/// refused or non-integer `_STA` says nothing, and refuses the declaration.
fn present<K: Kernel>(aml: &mut Aml<'_, K>, names: &Names, root: &str, census: &mut Census) -> Result<bool, String> {
    let path = format!("{root}._STA");
    if !names.paths.contains(&path) {
        return Ok(true);
    }
    match evaluate(aml, census, "_STA", &path, &[]) {
        Some(Value::Integer(status)) => Ok(status & PRESENT != 0),
        _ => Err(said(&path, "a root bridge's _STA says nothing")),
    }
}

/// `why`, with the path the firmware chose said on a line of its own.
fn said(path: &str, why: &str) -> String {
    println!("{OWN}{path}: {why}");
    why.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aml::load;
    use crate::host::tests::Scripted;

    const Q35_DSDT: &[u8] = include_bytes!("../../../lib/acpi/fixtures/qemu-11.1.1/dsdt.bin");
    const VIRT_DSDT: &[u8] = include_bytes!("../../../lib/acpi/fixtures/qemu-11.1.1-virt/dsdt.bin");

    /// The windows a DSDT's roots declare, read as the server reads them.
    fn declared(dsdt: &[u8]) -> Result<Vec<RootBridgeWindow>, String> {
        let kernel = crate::aml::tests::crafted(dsdt, &[]);
        let (_, kept) = load(&kernel, crate::aml::tests::CRAFTED_RSDP);
        let mut aml = kept.expect("the DSDT loads");
        let names = Names::read(&aml.interpreter).expect("its namespace walks");
        read(&mut aml, &names, &mut Census::default())
    }

    /// QEMU's `virt` (the measured case): its one root, `\_SB.PCI0`
    /// (PNP0A08), declares its 32-bit and 64-bit memory windows and nothing
    /// of its bus numbers, its I/O window translated to 0x3eff0000, ECAM or
    /// flash.
    #[test]
    fn virts_root_declares_its_two_memory_windows() {
        assert_eq!(
            declared(VIRT_DSDT),
            Ok(vec![
                RootBridgeWindow { base: 0x1000_0000, length: 0x2eff_0000 },
                RootBridgeWindow { base: 0x80_0000_0000, length: 0x80_0000_0000 },
            ])
        );
    }

    /// q35: one root whose `_CRS` names the VGA window, two 32-bit windows
    /// and the 64-bit window above memory, and steps over its configuration
    /// ports and I/O ranges.
    #[test]
    fn q35s_root_declares_its_memory_windows() {
        assert_eq!(
            declared(Q35_DSDT),
            Ok(vec![
                RootBridgeWindow { base: 0xa_0000, length: 0x2_0000 },
                RootBridgeWindow { base: 0x8000_0000, length: 0x6000_0000 },
                RootBridgeWindow { base: 0xf000_0000, length: 0xec0_0000 },
                RootBridgeWindow { base: 0xc0_0000_0000, length: 0x8_0000_0000 },
            ])
        );
    }

    /// No byte of either `_CRS` as evaluated panics the decoder or runs it
    /// away: each takes each of its 255 other values in turn, and both arms,
    /// windows and refusal, are reached.
    #[test]
    fn no_single_byte_mutation_of_a_roots_crs_panics_or_runs_away() {
        for dsdt in [Q35_DSDT, VIRT_DSDT] {
            let kernel = crate::aml::tests::crafted(dsdt, &[]);
            let (_, kept) = load(&kernel, crate::aml::tests::CRAFTED_RSDP);
            let mut aml = kept.expect("the DSDT loads");
            let Ok(Value::Buffer(crs)) = aml.interpreter.evaluate(&mut aml.host, "\\_SB_.PCI0._CRS", &[]) else {
                panic!("PCI0._CRS is a buffer");
            };
            let (mut refused, mut mutations) = (0u64, 0u64);
            for offset in 0..crs.len() {
                for value in (0..=255u8).filter(|&v| v != crs[offset]) {
                    let mut mutated = crs.clone();
                    mutated[offset] = value;
                    let mut out = [RootBridgeWindow::default(); MAX_WINDOWS];
                    match toyos_acpi::memory_windows(&mutated[..], 0, &mut out) {
                        Ok(count) => assert!(out[..count].iter().all(|w| w.length != 0), "byte {offset} as {value:#04x}"),
                        Err(_) => refused += 1,
                    }
                    mutations += 1;
                }
            }
            assert!(refused > 0 && refused < mutations, "{refused} of {mutations} refused");
        }
    }

    /// A root that is not a list of windows, one whose `_CRS` is refused, and
    /// a namespace with none: no declaration, each said by name; and what the
    /// server declares then is no window at all.
    #[test]
    fn a_root_that_declares_nothing_usable_refuses_the_whole_declaration() {
        use crate::aml::tests::{cat, sealed};
        // Device (PCI0) { Name (_HID, "PNP0A08") Name (_CRS, Buffer { IRQ }) }
        let device = |crs: &[u8]| {
            let body = cat(&[&[0x08], b"_HID", &[0x0D], b"PNP0A08", &[0x00], &[0x08], b"_CRS", crs]);
            let mut d = vec![0x5B, 0x82, (body.len() + 5) as u8];
            d.extend_from_slice(b"PCI0");
            d.extend_from_slice(&body);
            d
        };
        let irq = [0x11, 0x08, 0x0A, 0x05, 0x22, 0x01, 0x00, 0x79, 0x00];
        let refused = declared(&sealed(b"DSDT", &device(&irq)));
        assert_eq!(refused, Err("a root bridge's _CRS is no list of windows: descriptor tag 0x22 is not decoded here".into()));
        let integer = declared(&sealed(b"DSDT", &device(&[0x0A, 0x05])));
        assert_eq!(integer, Err("a root bridge's _CRS is no buffer".into()));
        assert_eq!(declared(&sealed(b"DSDT", &[0x08, b'A', b'A', b'A', b'A', 0x00])), Err("the namespace names no PCI host bridge".into()));

        let kernel = Scripted::default();
        declare(&kernel, Err("the namespace names no PCI host bridge".into())).expect("not stopping");
        assert_eq!(*kernel.declared.borrow(), Some(Vec::new()));
    }
}
