//! The PCI root bridges, declared to the kernel once ([`Kernel::declare`]):
//! each one's segment, buses and memory windows, and what its `_OSC`
//! offers. Every PCI claim on the machine waits for the declaration, so it
//! is read as soon as the namespace is initialised (ACPI 6.5 §6.5.1).
//!
//! **A root is a PCI host bridge by its `_HID` or a `_CID`** (PNP0A03 or
//! PNP0A08, §6.1.5, §6.1.2) **below no other one**; a root whose `_STA` says
//! it is not present decodes nothing and is not read. Its segment is its
//! `_SEG` (§6.5.6), 0 without one; its buses and windows are what
//! `toyos_acpi::root_bridge` reads of its `_CRS` (§6.2.2); its `_OSC` is
//! negotiated, and what it offers and what a commit was granted recorded
//! (`crate::osc::negotiate_root`).
//!
//! **Native hot-plug is committed only where the image declares every
//! hot-plug port internal** ([`PORTS_INTERNAL`]), and even then not on a root
//! with a port its firmware marks external-facing, nor one whose port's
//! `_DSD` cannot be read ([`external_facing`]). The owner's "refuse external"
//! holds and no claim behind an external port is refused in code yet, and no
//! firmware signal says a port is internal: ACPI 6.5 defines no such
//! property (§6.2.5 only frames `_DSD`), and Microsoft's `ExternalFacingPort`
//! ("Kernel DMA Protection", Microsoft Learn) marks the external ports alone,
//! so a port without it is unknown, not internal.
//!
//! **The declaration is every root together, or none at all**: a root whose
//! `_STA`, `_SEG` or `_CRS` is refused, a list the decoder refuses or that
//! names no bus range, more roots or windows than a declaration carries, and
//! a namespace with no root are each said by name and declared as no root,
//! which the kernel refuses — and with it every PCI claim, rather than one
//! placed by a list with a hole in it. A machine with no namespace declares
//! the same. An `_OSC` refused is that root's offer refused, and nothing
//! else's.

use toyos_abi::acpi::{Osc, RootBridgeWindow, MAX_BRIDGES, MAX_WINDOWS};
use toyos_aml::{Kind, Value};

use crate::aml::{kind, Aml};
use crate::host::{Kernel, RootsDeclared, Stopping, OWN};
use crate::init::{evaluate, host_bridge, Census, Names, PRESENT};

/// One PCI root bridge, as it is declared.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Root {
    pub segment: u16,
    pub buses: (u8, u8),
    pub windows: Vec<RootBridgeWindow>,
    pub osc: Osc,
    /// What a commit of native hot-plug was granted, where one was made.
    pub committed: Option<u32>,
}

/// The argument an image's acpiserver row carries to declare every hot-plug
/// port below the machine's roots internal; only the hot-plug test's image,
/// whose ports are QEMU's own, carries it.
pub const PORTS_INTERNAL: &str = "--hot-plug-ports-internal";

/// Every root bridge, in walk order, or why there is no declaration;
/// `internal` is the image's [`PORTS_INTERNAL`].
pub fn read<K: Kernel>(aml: &mut Aml<'_, K>, names: &Names, census: &mut Census, internal: bool) -> Result<Vec<Root>, String> {
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
    let mut roots: Vec<Root> = Vec::new();
    let mut room = MAX_WINDOWS;
    for root in bridges {
        if !present(aml, names, root, census)? {
            println!("{OWN}{root} is not present, so it decodes nothing");
            continue;
        }
        if roots.len() == MAX_BRIDGES {
            return Err(format!("the namespace names more than the {MAX_BRIDGES} root bridges a declaration carries"));
        }
        let segment = segment(aml, names, root)?;
        let path = format!("{root}._CRS");
        let crs = match aml.interpreter.evaluate(&mut aml.host, &path, &[]) {
            Ok(Value::Buffer(crs)) => crs,
            Ok(_) => return Err(said(&path, "a root bridge's _CRS is no buffer")),
            Err(why) => return Err(said(&path, &format!("a root bridge's _CRS was refused: {}", kind(&why)))),
        };
        let bytes: Vec<String> = crs.iter().map(|b| format!("{b:02x}")).collect();
        println!("{OWN}{path}: {}", bytes.join(" "));
        let mut windows = [RootBridgeWindow::default(); MAX_WINDOWS];
        let found = match toyos_acpi::root_bridge(&crs[..], 0, &mut windows[..room]) {
            Ok(found) => found,
            Err(why) => return Err(said(&path, &format!("a root bridge's _CRS is no list of windows: {why}"))),
        };
        let buses = found.buses.ok_or_else(|| said(&path, "a root bridge's _CRS names no bus range"))?;
        room -= found.windows;
        let commit = match external_ports(aml, names, root) {
            _ if !internal => {
                println!("{OWN}{root}'s hot-plug ports are not declared internal, so native hot-plug is not committed");
                false
            }
            Ok(external) if external.is_empty() => true,
            Ok(external) => {
                println!("{OWN}{root} has external-facing ports ({}), so native hot-plug is not committed", external.join(", "));
                false
            }
            Err(why) => {
                println!("{OWN}{root}'s ports are not known to be internal ({why}), so native hot-plug is not committed");
                false
            }
        };
        let (osc, committed) = osc(aml, names, root, commit);
        roots.push(Root { segment, buses, windows: windows[..found.windows].to_vec(), osc, committed });
    }
    let counts: Vec<String> = roots.iter().map(|root| root.windows.len().to_string()).collect();
    println!("acpiserver: {} root bridge(s), with {} memory window(s)", roots.len(), counts.join(" + "));
    Ok(roots)
}

/// What `root`'s `_OSC` offers and what a commit was granted, said under
/// [`OWN`] where it was refused.
fn osc<K: Kernel>(aml: &mut Aml<'_, K>, names: &Names, root: &str, commit: bool) -> (Osc, Option<u32>) {
    let path = format!("{root}._OSC");
    if !names.paths.contains(&path) {
        return (Osc::Absent, None);
    }
    match crate::osc::negotiate_root(aml, &path, commit) {
        Ok((offered, committed)) => (Osc::Offered(offered), committed),
        Err(why) => {
            println!("{OWN}{path} was refused: {why}");
            (Osc::Refused, None)
        }
    }
}

/// `root`'s segment: its `_SEG`, 0 where it has none (ACPI 6.5 §6.5.6).
fn segment<K: Kernel>(aml: &mut Aml<'_, K>, names: &Names, root: &str) -> Result<u16, String> {
    let path = format!("{root}._SEG");
    if !names.paths.contains(&path) {
        return Ok(0);
    }
    match aml.interpreter.evaluate(&mut aml.host, &path, &[]) {
        Ok(Value::Integer(segment)) => u16::try_from(segment).map_err(|_| said(&path, "a root bridge's _SEG is past 0xffff")),
        Ok(_) => Err(said(&path, "a root bridge's _SEG is no integer")),
        Err(why) => Err(said(&path, &format!("a root bridge's _SEG was refused: {}", kind(&why)))),
    }
}

/// Declare what [`read`] answers, or no root where it refused, saying which
/// and what the kernel did with it.
pub fn declare<K: Kernel>(kernel: &K, read: Result<Vec<Root>, String>) -> Result<(), Stopping> {
    let roots = match read {
        Ok(roots) => roots,
        Err(why) => {
            println!("acpiserver: no root bridge declared, so every PCI claim is refused: {why}");
            Vec::new()
        }
    };
    let listed: Vec<String> =
        roots.iter().flat_map(|root| &root.windows).map(|w| format!("mem {:#x}..{:#x}", w.base, w.end())).collect();
    match kernel.declare(&roots)? {
        RootsDeclared::Held => println!("acpiserver: the root bridges' memory declared: {}", listed.join(", ")),
        RootsDeclared::Refused if roots.is_empty() => {}
        RootsDeclared::Refused => println!("acpiserver: the kernel refused the root bridges' memory as declared ({}); its log says why", listed.join(", ")),
        RootsDeclared::Late => println!("acpiserver: the kernel already holds an answer for the root bridges' memory, so nothing is declared"),
    }
    Ok(())
}

/// Microsoft's `_DSD` UUID for a PCIe root port's `ExternalFacingPort`
/// property ("Kernel DMA Protection", Microsoft Learn): a port a device
/// outside the machine plugs into, such as a Thunderbolt one.
const EXTERNAL_FACING_UUID: [u8; 16] = crate::osc::uuid(b"EFCC06CC-73AC-4BC3-BFF0-76143807C389");

/// The devices directly below `root` whose `_DSD` says they face outside the
/// machine; an error where one's `_DSD` is refused or cannot be read.
fn external_ports<K: Kernel>(aml: &mut Aml<'_, K>, names: &Names, root: &str) -> Result<Vec<String>, String> {
    let ports: Vec<String> = names
        .objects
        .iter()
        .filter(|named| named.kind == Kind::Device && below(&named.path, root) && !named.path[root.len() + 1..].contains('.'))
        .filter(|named| names.paths.contains(&format!("{}._DSD", named.path)))
        .map(|named| named.path.clone())
        .collect();
    let mut external = Vec::new();
    for port in ports {
        let path = format!("{port}._DSD");
        let dsd = aml.interpreter.evaluate(&mut aml.host, &path, &[]).map_err(|why| said(&path, &format!("refused: {}", kind(&why))))?;
        if external_facing(&dsd).map_err(|why| said(&path, &why))? {
            external.push(port);
        }
    }
    Ok(external)
}

/// Whether a `_DSD` package (ACPI 6.5 §6.2.5) says its device is an external
/// facing port: a property `ExternalFacingPort` of non-zero value under
/// [`EXTERNAL_FACING_UUID`]. A package that is not UUID-and-package pairs, or
/// whose property is no integer, is refused rather than taken as internal.
pub fn external_facing(dsd: &Value) -> Result<bool, String> {
    let Value::Package(pairs) = dsd else { return Err("a _DSD is no package".into()) };
    if pairs.len() % 2 != 0 {
        return Err("a _DSD is not UUID and package pairs".into());
    }
    for pair in pairs.chunks(2) {
        let (Value::Buffer(uuid), Value::Package(properties)) = (&pair[0], &pair[1]) else {
            return Err("a _DSD is not UUID and package pairs".into());
        };
        if uuid[..] != EXTERNAL_FACING_UUID {
            continue;
        }
        for property in properties {
            match property {
                Value::Package(kv) if kv.len() == 2 && kv[0] == Value::String(b"ExternalFacingPort".to_vec()) => match kv[1] {
                    Value::Integer(value) => return Ok(value != 0),
                    _ => return Err("its ExternalFacingPort is no integer".into()),
                },
                Value::Package(kv) if kv.len() == 2 => {}
                _ => return Err("a _DSD property is no key and value".into()),
            }
        }
    }
    Ok(false)
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
    use crate::aml::tests::{sealed, CRAFTED_RSDP};
    use crate::common::{buffer, cat, def_name, device, int, method, package, ret, string};
    use crate::host::tests::Scripted;
    use crate::osc::ROOT_CONTROL;

    const Q35_DSDT: &[u8] = include_bytes!("../../../lib/acpi/fixtures/qemu-11.1.1/dsdt.bin");
    const VIRT_DSDT: &[u8] = include_bytes!("../../../lib/acpi/fixtures/qemu-11.1.1-virt/dsdt.bin");

    /// The roots a DSDT declares, read as the server reads them.
    fn declared(dsdt: &[u8]) -> Result<Vec<Root>, String> {
        let kernel = crate::aml::tests::crafted(dsdt, &[]);
        let (_, kept) = load(&kernel, CRAFTED_RSDP);
        let mut aml = kept.expect("the DSDT loads");
        let names = Names::read(&aml.interpreter).expect("its namespace walks");
        read(&mut aml, &names, &mut Census::default(), true)
    }

    fn windows(root: &Root) -> Vec<RootBridgeWindow> {
        root.windows.clone()
    }

    /// QEMU's `virt` (the measured case): its one root, `\_SB.PCI0`
    /// (PNP0A08) on segment 0, declares buses 0 to 0xff and its 32-bit and
    /// 64-bit memory windows, and nothing of its I/O window translated to
    /// 0x3eff0000, ECAM or flash; its `_OSC` grants what was asked.
    #[test]
    fn virts_root_declares_its_buses_windows_and_grant() {
        let roots = declared(VIRT_DSDT).expect("virt's root");
        let [root] = roots.as_slice() else { panic!("{roots:x?}") };
        assert_eq!((root.segment, root.buses), (0, (0, 0xff)));
        assert_eq!(
            windows(root),
            [
                RootBridgeWindow { base: 0x1000_0000, length: 0x2eff_0000 },
                RootBridgeWindow { base: 0x80_0000_0000, length: 0x80_0000_0000 },
            ]
        );
        assert_eq!(root.osc, Osc::Offered(VIRT_OFFERED));
    }

    /// q35: one root on buses 0 to 0xff whose `_CRS` names the VGA window,
    /// two 32-bit windows and the 64-bit window above memory, and steps over
    /// its configuration ports and I/O ranges.
    #[test]
    fn q35s_root_declares_its_buses_windows_and_grant() {
        let roots = declared(Q35_DSDT).expect("q35's root");
        let [root] = roots.as_slice() else { panic!("{roots:x?}") };
        assert_eq!((root.segment, root.buses), (0, (0, 0xff)));
        assert_eq!(
            windows(root),
            [
                RootBridgeWindow { base: 0xa_0000, length: 0x2_0000 },
                RootBridgeWindow { base: 0x8000_0000, length: 0x6000_0000 },
                RootBridgeWindow { base: 0xf000_0000, length: 0xec0_0000 },
                RootBridgeWindow { base: 0xc0_0000_0000, length: 0x8_0000_0000 },
            ]
        );
        assert_eq!(root.osc, Osc::Offered(Q35_OFFERED));
    }

    /// What each QEMU's root `_OSC` query offers of [`ROOT_CONTROL`]: `virt`
    /// both, q35 the capability structure and not native hot-plug, which its
    /// firmware keeps for its own ACPI hot-plug.
    const VIRT_OFFERED: u32 = ROOT_CONTROL;
    const Q35_OFFERED: u32 = toyos_abi::acpi::CAPABILITY_STRUCTURE;

    /// No byte of either `_CRS` as evaluated panics the decoder or runs it
    /// away: each takes each of its 255 other values in turn, and both arms,
    /// a root and a refusal, are reached.
    #[test]
    fn no_single_byte_mutation_of_a_roots_crs_panics_or_runs_away() {
        for dsdt in [Q35_DSDT, VIRT_DSDT] {
            let kernel = crate::aml::tests::crafted(dsdt, &[]);
            let (_, kept) = load(&kernel, CRAFTED_RSDP);
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
                    match toyos_acpi::root_bridge(&mutated[..], 0, &mut out) {
                        Ok(found) => assert!(out[..found.windows].iter().all(|w| w.length != 0), "byte {offset} as {value:#04x}"),
                        Err(_) => refused += 1,
                    }
                    mutations += 1;
                }
            }
            assert!(refused > 0 && refused < mutations, "{refused} of {mutations} refused");
        }
    }

    /// A root's `_CRS`: buses 0 to 0xff and one window at 0x80000000.
    fn crs() -> Vec<u8> {
        crs_of(&[(0x8000_0000, 0x1000_0000)])
    }

    /// A root's `_CRS`: buses 0 to 0xff and a DWord window at each
    /// `(base, length)`.
    fn crs_of(windows: &[(u32, u32)]) -> Vec<u8> {
        let bus = [0x88, 0x0D, 0x00, 0x02, 0x0C, 0x00, 0, 0, 0, 0, 0xFF, 0, 0, 0, 0x00, 0x01];
        let mut list = bus.to_vec();
        for &(base, length) in windows {
            list.extend_from_slice(&[0x87, 0x17, 0x00, 0x00, 0x0C, 0x03]);
            for field in [0u32, base, base + (length - 1), 0, length] {
                list.extend_from_slice(&field.to_le_bytes());
            }
        }
        list.extend_from_slice(&[0x79, 0x00]);
        buffer(&int(list.len() as u64), &list)
    }

    /// A root with many windows is declared whole: the bound is the
    /// declaration's, not a root's. The legacy windows a laptop's firmware
    /// lists 16 KiB each from 0xc0000, a low and a high window, 18 in all,
    /// more than the AMD bench's 16; and a root past the declaration's own
    /// bound is refused by name.
    #[test]
    fn a_root_with_many_windows_is_declared_and_only_an_absurd_one_refused() {
        let legacy = (0..16u32).map(|i| (0xc_0000 + i * 0x4000, 0x4000));
        let many: Vec<(u32, u32)> = legacy.chain([(0xa_0000, 0x2_0000), (0xd000_0000, 0x2000_0000)]).collect();
        let roots = declared(&root(&crs_of(&many), &[])).expect("eighteen windows");
        assert_eq!(roots[0].windows.len(), 18);
        let absurd: Vec<(u32, u32)> = (0..MAX_WINDOWS as u32 + 1).map(|i| (0x1000_0000 + i * 0x1000, 0x1000)).collect();
        assert_eq!(
            declared(&root(&crs_of(&absurd), &[])),
            Err(format!("a root bridge's _CRS is no list of windows: more memory windows or I/O runs than the {MAX_WINDOWS} there is room for"))
        );
    }

    /// `\_SB.PCI0`, a PNP0A08 whose `_CRS` is `crs` and which carries `rest`.
    fn root(crs: &[u8], rest: &[u8]) -> Vec<u8> {
        let body = cat(&[&def_name("_HID", &string("PNP0A08")), &def_name("_CRS", crs), rest]);
        sealed(b"DSDT", &crate::common::scope("\\_SB", &device("PCI0", &body)))
    }

    /// A root's `_OSC` returning `answer`.
    fn osc(answer: &[u8]) -> Vec<u8> {
        method("_OSC", 4, &ret(answer))
    }

    /// The three DWORDs as a Buffer.
    fn dwords(words: [u32; 3]) -> Vec<u8> {
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        buffer(&int(12), &bytes)
    }

    /// What a root's `_OSC` offers and what its commit was granted, read off a
    /// namespace of that one root: a firmware that echoes the request offers
    /// it and grants the commit; one that answers failure, or masks every
    /// Control bit, offers nothing and is committed nothing; Control bits
    /// offered that were never asked for are dropped; a root with no `_OSC`
    /// says so; and an answer that is no buffer of three DWORDs is refused by
    /// name, its root still declared.
    #[test]
    fn a_roots_osc_is_negotiated_and_its_offer_read() {
        let negotiated = |rest: &[u8]| declared(&root(&crs(), rest)).map(|roots| (roots[0].osc, roots[0].committed));
        // Return (Arg3): the query's DWORDs back, and the commit's.
        assert_eq!(negotiated(&osc(&[0x6B])), Ok((Osc::Offered(ROOT_CONTROL), Some(ROOT_CONTROL))));
        assert_eq!(negotiated(&osc(&dwords([1 << 1, 0, ROOT_CONTROL]))), Ok((Osc::Offered(0), None)));
        assert_eq!(negotiated(&osc(&dwords([1 << 3, 0, ROOT_CONTROL]))), Ok((Osc::Offered(0), None)));
        assert_eq!(negotiated(&osc(&dwords([0, 0, 0]))), Ok((Osc::Offered(0), None)));
        assert_eq!(negotiated(&osc(&dwords([0, 0, u32::MAX]))), Ok((Osc::Offered(ROOT_CONTROL), Some(ROOT_CONTROL))));
        // QEMU q35's own shape: the Control DWORD masked by the firmware,
        // 0x1F with `acpi-pci-hotplug-with-bridge-support=off`, 0x1E with
        // its default, which keeps native hot-plug: half of what the kernel's
        // slots need is no commit.
        let masked = |mask: u8| {
            let ctrl = cat(&[&[0x8A, 0x6B, 0x0A, 0x08], b"CTRL", &[0x7B], b"CTRL", &[0x0A, mask], b"CTRL"]);
            method("_OSC", 4, &cat(&[&ctrl, &ret(&[0x6B])]))
        };
        assert_eq!(negotiated(&masked(0x1F)), Ok((Osc::Offered(ROOT_CONTROL), Some(ROOT_CONTROL))));
        assert_eq!(negotiated(&masked(0x1E)), Ok((Osc::Offered(toyos_abi::acpi::CAPABILITY_STRUCTURE), None)));
        assert_eq!(negotiated(&[]), Ok((Osc::Absent, None)));
        assert_eq!(negotiated(&osc(&buffer(&int(4), &[0; 4]))), Ok((Osc::Refused, None)));
        assert_eq!(negotiated(&osc(&int(5))), Ok((Osc::Refused, None)));
    }

    /// The `_OSC` is queried with the Query flag set and then committed with
    /// it clear, twice in all, where the query offered native hot-plug with
    /// the capability structure. The firmware here counts its calls and
    /// keeps the first DWORD it was last passed.
    #[test]
    fn a_roots_osc_offering_hot_plug_is_committed() {
        let body = cat(&[
            &[0x75],
            b"CNT_",
            &[0x8A, 0x6B, 0x00],
            b"STS0",
            &[0x70],
            b"STS0",
            b"QFLG",
            &ret(&[0x6B]),
        ]);
        let rest = cat(&[&def_name("CNT_", &int(0)), &def_name("QFLG", &int(0xFF)), &method("_OSC", 4, &body)]);
        let kernel = crate::aml::tests::crafted(&root(&crs(), &rest), &[]);
        let (_, kept) = load(&kernel, CRAFTED_RSDP);
        let mut aml = kept.expect("the DSDT loads");
        let names = Names::read(&aml.interpreter).expect("its namespace walks");
        let roots = read(&mut aml, &names, &mut Census::default(), true).expect("one root");
        assert_eq!((roots[0].osc, roots[0].committed), (Osc::Offered(ROOT_CONTROL), Some(ROOT_CONTROL)));
        let integer = |aml: &mut Aml<'_, Scripted>, path| aml.interpreter.evaluate(&mut aml.host, path, &[]);
        assert!(matches!(integer(&mut aml, "\\_SB_.PCI0.CNT_"), Ok(Value::Integer(2))));
        assert!(matches!(integer(&mut aml, "\\_SB_.PCI0.QFLG"), Ok(Value::Integer(0))));
    }

    /// A Thunderbolt root port's `_DSD`, as Microsoft's Kernel DMA
    /// Protection page gives it: external facing; the same with a value of 0,
    /// or under another UUID, or with no such property: not; and a package
    /// that is not UUID-and-package pairs, or whose value is no integer:
    /// refused, never taken as internal.
    #[test]
    fn an_external_facing_port_is_read_off_its_dsd() {
        let property = |key: &str, value: Value| Value::Package(vec![Value::String(key.as_bytes().to_vec()), value]);
        let dsd = |uuid: [u8; 16], properties: Vec<Value>| Value::Package(vec![Value::Buffer(uuid.to_vec()), Value::Package(properties)]);
        let thunderbolt = dsd(EXTERNAL_FACING_UUID, vec![property("ExternalFacingPort", Value::Integer(1)), property("UID", Value::Integer(0))]);
        assert_eq!(external_facing(&thunderbolt), Ok(true));
        assert_eq!(external_facing(&dsd(EXTERNAL_FACING_UUID, vec![property("ExternalFacingPort", Value::Integer(0))])), Ok(false));
        assert_eq!(external_facing(&dsd(EXTERNAL_FACING_UUID, vec![property("UID", Value::Integer(0))])), Ok(false));
        let other = crate::osc::uuid(b"DAFFD814-6EBA-4D8C-8A91-BC9BBF4AA301");
        assert_eq!(external_facing(&dsd(other, vec![property("ExternalFacingPort", Value::Integer(1))])), Ok(false));
        assert!(external_facing(&Value::Integer(1)).is_err());
        assert!(external_facing(&Value::Package(vec![Value::Buffer(EXTERNAL_FACING_UUID.to_vec())])).is_err());
        assert!(external_facing(&dsd(EXTERNAL_FACING_UUID, vec![property("ExternalFacingPort", Value::String(b"1".to_vec()))])).is_err());
        assert!(external_facing(&dsd(EXTERNAL_FACING_UUID, vec![Value::Integer(1)])).is_err());
    }

    /// A root whose firmware offers native hot-plug is committed it only
    /// where no port below it faces outside the machine: a Thunderbolt-like
    /// port's `_DSD` keeps the slot the firmware's, and one that cannot be
    /// read keeps it too.
    #[test]
    fn a_root_with_an_external_facing_port_is_not_committed() {
        let uuid_bytes = EXTERNAL_FACING_UUID.to_vec();
        let port = |value: u64| {
            let property = package(&[string("ExternalFacingPort"), int(value)]);
            let dsd = package(&[buffer(&int(16), &uuid_bytes), package(&[property])]);
            device("RP01", &cat(&[&def_name("_ADR", &int(0x0007_0000)), &def_name("_DSD", &dsd)]))
        };
        let negotiated = |rest: &[u8]| declared(&root(&crs(), rest)).map(|roots| (roots[0].osc, roots[0].committed));
        let rest = |port: &[u8]| cat(&[&osc(&[0x6B]), port]);
        assert_eq!(negotiated(&rest(&port(1))), Ok((Osc::Offered(ROOT_CONTROL), None)));
        assert_eq!(negotiated(&rest(&port(0))), Ok((Osc::Offered(ROOT_CONTROL), Some(ROOT_CONTROL))));
        let unreadable = device("RP01", &def_name("_DSD", &int(1)));
        assert_eq!(negotiated(&rest(&unreadable)), Ok((Osc::Offered(ROOT_CONTROL), None)));
    }

    /// An image that declares no port internal commits nothing, whatever the
    /// firmware offers: no firmware property says a port is internal.
    #[test]
    fn a_root_is_not_committed_unless_its_ports_are_declared_internal() {
        let kernel = crate::aml::tests::crafted(&root(&crs(), &osc(&[0x6B])), &[]);
        let (_, kept) = load(&kernel, CRAFTED_RSDP);
        let mut aml = kept.expect("the DSDT loads");
        let names = Names::read(&aml.interpreter).expect("its namespace walks");
        let roots = read(&mut aml, &names, &mut Census::default(), false).expect("one root");
        assert_eq!((roots[0].osc, roots[0].committed), (Osc::Offered(ROOT_CONTROL), None));
    }

    /// A root that is not a list of windows, one whose `_CRS` is refused or
    /// names no bus range, and a namespace with none: no declaration, each
    /// said by name; and what the server declares then is no root at all.
    #[test]
    fn a_root_that_declares_nothing_usable_refuses_the_whole_declaration() {
        let irq = buffer(&int(5), &[0x22, 0x01, 0x00, 0x79, 0x00]);
        assert_eq!(declared(&root(&irq, &[])), Err("a root bridge's _CRS is no list of windows: descriptor tag 0x22 is not decoded here".into()));
        assert_eq!(declared(&root(&int(5), &[])), Err("a root bridge's _CRS is no buffer".into()));
        let no_bus = buffer(&int(2), &[0x79, 0x00]);
        assert_eq!(declared(&root(&no_bus, &[])), Err("a root bridge's _CRS names no bus range".into()));
        assert_eq!(declared(&sealed(b"DSDT", &def_name("AAAA", &int(0)))), Err("the namespace names no PCI host bridge".into()));

        let kernel = Scripted::default();
        declare(&kernel, Err("the namespace names no PCI host bridge".into())).expect("not stopping");
        assert_eq!(*kernel.roots.borrow(), Some(Vec::new()));
    }
}
