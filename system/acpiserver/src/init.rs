//! The namespace initialised (ACPI 6.5 §6.5.1, §6.5.4): what OSPM runs of a
//! machine's AML once its tables are loaded, and again over what a table
//! loaded at run time added.
//!
//! **`_REG(2, 1)` first**, in each scope that declares a PCI_Config region
//! not on a root bus naming a `_BBN`: §6.5.4 makes those regions available
//! once the bus is configured, and the kernel reads configuration space from
//! before this server runs, so they are announced before any `_INI` reaches
//! them. A region on a root bus naming a `_BBN` is always available, and its
//! scope's `_REG` is not run. Finding the host bridge of a root bus that
//! names none evaluates its candidates' `_HID` and `_CID`, as the
//! interpreter's own access to such a region does.
//!
//! **Then `\_SB._INI`, unconditionally** ("at the beginning of namespace
//! initialization"), **then each device's `_STA` and `_INI` by Table 6.67,
//! where an `_INI` is defined at or below it**, parent before child: a device
//! its `_STA` says is present has its `_INI` run and its children examined;
//! one functional and not present has its children examined alone; one
//! neither, nothing below it. A device with no `_STA` is both. A Processor
//! object is a device here. After a run-time load, the `_INI`s that load
//! defined are run, by the same table, their ancestors' `_STA` asked again.
//!
//! **The walk looks for `_INI` methods** (§6.5.1: OSPM "will examine each of
//! the children of the device for \_INI methods", and evaluates `_STA`
//! "before evaluating the \_INI object"): a device with no `_INI` at or
//! below it is not examined, and its `_STA` is asked by what serves it, a
//! battery's once the controller is attached ([`crate::battery`]). **A
//! `_STA` refused, or no Integer, says nothing**, of which §6.5.1 is silent:
//! by ToyOS's rule its device's `_INI` is not run, since nothing said it is
//! present, and its children are examined, as Table 6.67's row that runs
//! nothing of a device yet closes nothing below it has them.
//!
//! **EmbeddedControl is announced in each scope that declares a region of
//! it once the controller is attached** ([`announce`], `crate::devices`),
//! after the walk. SystemMemory and SystemIO are always available and
//! announced to nobody, and no other space is served.
//!
//! An evaluation refused is said the first time by kind, its path under
//! [`OWN`], and counted; each device's `_STA` is said under [`OWN`] too, the
//! record a reading of the same machine under another OS is held to.

use std::collections::{BTreeMap, BTreeSet};

use toyos_aml::{Interpreter, Kind, Value};

use crate::aml::{kind, Aml};
use crate::host::{Kernel, OWN};
use crate::ledger::Ledger;

/// One object of the namespace, as one walk read it.
pub struct Named {
    pub path: String,
    pub depth: u32,
    pub serial: u32,
    pub kind: Kind,
}

/// The namespace read once, before anything is evaluated in it: every object
/// in walk order, and every path.
pub struct Names {
    pub objects: Vec<Named>,
    pub paths: BTreeSet<String>,
}

impl Names {
    pub fn read(interpreter: &Interpreter) -> Result<Names, String> {
        let mut walk = interpreter.walk().map_err(|why| kind(&why))?;
        let (mut objects, mut paths) = (Vec::new(), BTreeSet::new());
        while let Some(entry) = walk.next() {
            let path = walk.path().to_string();
            paths.insert(path.clone());
            objects.push(Named { path, depth: entry.depth, serial: entry.serial, kind: entry.kind });
        }
        Ok(Names { objects, paths })
    }

    fn has(&self, scope: &str, name: &str) -> bool {
        self.paths.contains(&format!("{scope}.{name}"))
    }

    fn devices(&self) -> BTreeSet<&str> {
        self.objects.iter().filter(|named| is_device(named.kind)).map(|named| named.path.as_str()).collect()
    }
}

pub fn is_device(kind: Kind) -> bool {
    matches!(kind, Kind::Device | Kind::Processor { .. })
}

/// The scope an object's path is in.
pub fn parent(path: &str) -> &str {
    path.rsplit_once('.').map_or("\\", |(scope, _)| scope)
}

/// `_STA`'s Present and Functional bits (§6.3.7).
pub const PRESENT: u64 = 1 << 0;
const FUNCTIONAL: u64 = 1 << 3;

/// What the initialisation ran, and what of it was refused.
#[derive(Default)]
pub struct Census {
    /// `\_SB._INI`: absent, run, or refused.
    pub sb_ini: Option<bool>,
    pub sta: u64,
    pub ini: u64,
    /// `_REG(space, 1)` runs, by space.
    pub reg: BTreeMap<u8, u64>,
    /// Each refusal, by the method's name and its kind.
    pub refused: Ledger,
    /// The longest time an evaluation asked to wait, refused or not, in µs
    /// (`toyos_aml::Usage::waited_us`).
    pub longest_wait_us: u64,
}

impl Census {
    /// The line that says what ran, a count each.
    pub fn line(&self) -> String {
        let sb = match self.sb_ini {
            None => "absent",
            Some(true) => "run",
            Some(false) => "refused",
        };
        let reg: Vec<String> = self.reg.iter().map(|(space, n)| format!("_REG({space}, 1) in {n} scope(s)")).collect();
        let refused = if self.refused.is_empty() { "none".to_string() } else { self.refused.counts() };
        format!(
            "acpiserver: the namespace initialised: \\_SB._INI {sb}; {} _STA read, {} _INI run, {}; the longest wait an evaluation asked {} µs; evaluations refused, by kind: {refused}",
            self.sta,
            self.ini,
            if reg.is_empty() { "no _REG run".to_string() } else { reg.join(", ") },
            self.longest_wait_us
        )
    }

    fn refuse(&mut self, method: &str, path: &str, why: &toyos_aml::Error) {
        let what = format!("{method}: {}", kind(why));
        if self.refused.see(&what) {
            println!("acpiserver: an evaluation of the initialisation was refused for the first time: {what}");
        }
        println!("{OWN}{path} was refused {why:x?}");
    }
}

/// Evaluate `path`, counted in `census` under `method` where refused.
pub fn evaluate<K: Kernel>(aml: &mut Aml<'_, K>, census: &mut Census, method: &str, path: &str, args: &[Value]) -> Option<Value> {
    let answer = aml.interpreter.evaluate(&mut aml.host, path, args);
    census.longest_wait_us = census.longest_wait_us.max(aml.interpreter.usage().waited_us);
    match answer {
        Ok(value) => Some(value),
        Err(_) if aml.host.stopping => None,
        Err(why) => {
            census.refuse(method, path, &why);
            None
        }
    }
}

/// Initialise the namespace the load kept: every scope's PCI_Config
/// announced, `\_SB._INI`, then the walk.
pub fn initialise<K: Kernel>(aml: &mut Aml<'_, K>, names: &Names, census: &mut Census) {
    announce_config(aml, names, 0, census);
    if names.paths.contains("\\_SB_._INI") {
        census.sb_ini = Some(evaluate(aml, census, "\\_SB._INI", "\\_SB_._INI", &[]).is_some());
    }
    walk(aml, names, 0, census);
}

/// What a run-time load added since `since`: its PCI_Config regions
/// announced, its `_INI`s run by Table 6.67, and its EmbeddedControl regions
/// announced where the controller is attached.
pub fn loaded<K: Kernel>(aml: &mut Aml<'_, K>, names: &Names, since: u32, census: &mut Census) {
    announce_config(aml, names, since, census);
    walk(aml, names, since, census);
    if aml.host.ec.is_some() {
        announce(aml, names, EMBEDDED_CONTROL, since, census);
    }
}

/// The EmbeddedControl space's number (Table 5.182).
pub const EMBEDDED_CONTROL: u8 = 3;
const PCI_CONFIG: u8 = 2;

/// Each scope that declares a region of `space` defined at `since` or after,
/// once, in walk order.
fn scopes(names: &Names, space: u8, since: u32) -> Vec<&str> {
    let mut seen = BTreeSet::new();
    names
        .objects
        .iter()
        .filter(|named| named.serial >= since && named.kind == Kind::OperationRegion { space })
        .map(|named| parent(&named.path))
        .filter(|scope| seen.insert(*scope))
        .collect()
}

/// `_REG(space, 1)` in each scope that declares a region of `space` defined
/// at `since` or after and names a `_REG`.
pub fn announce<K: Kernel>(aml: &mut Aml<'_, K>, names: &Names, space: u8, since: u32, census: &mut Census) {
    for scope in scopes(names, space, since) {
        reg(aml, names, space, scope, census);
    }
}

fn reg<K: Kernel>(aml: &mut Aml<'_, K>, names: &Names, space: u8, scope: &str, census: &mut Census) {
    if !names.has(scope, "_REG") || aml.host.stopping {
        return;
    }
    if evaluate(aml, census, "_REG", &format!("{scope}._REG"), &[Value::Integer(space.into()), Value::Integer(1)]).is_some() {
        *census.reg.entry(space).or_insert(0) += 1;
    }
}

/// `_REG(2, 1)` in each scope whose PCI_Config regions are not on a root bus
/// naming a `_BBN`, and below a host bridge.
fn announce_config<K: Kernel>(aml: &mut Aml<'_, K>, names: &Names, since: u32, census: &mut Census) {
    let devices = names.devices();
    for scope in scopes(names, PCI_CONFIG, since) {
        // The device the region addresses, and each one above it.
        let above: Vec<&str> = std::iter::successors(Some(scope), |path| (*path != "\\").then(|| parent(path)))
            .filter(|path| devices.contains(path))
            .collect();
        let Some(&device) = above.first() else { continue };
        let mut bridge = None;
        for (at, candidate) in above.iter().enumerate() {
            if names.has(candidate, "_BBN") {
                bridge = Some((at, true));
                break;
            }
            if host_bridge(aml, names, candidate, census) {
                bridge = Some((at, false));
                break;
            }
        }
        match bridge {
            // The host bridge itself, or a device on the bus right below it.
            Some((at, true)) if at <= 1 => {}
            Some(_) => reg(aml, names, PCI_CONFIG, scope, census),
            None => println!("{OWN}{device} declares a PCI_Config region below no host bridge"),
        }
    }
}

/// Whether `device` is a PCI host bridge by its `_HID` or a `_CID` (§6.1.5,
/// §6.1.2): PNP0A03 or PNP0A08.
pub fn host_bridge<K: Kernel>(aml: &mut Aml<'_, K>, names: &Names, device: &str, census: &mut Census) -> bool {
    for id in ["_HID", "_CID"] {
        if !names.has(device, id) {
            continue;
        }
        let ids = match evaluate(aml, census, id, &format!("{device}.{id}"), &[]) {
            Some(Value::Package(ids)) => ids,
            Some(id) => vec![id],
            None => continue,
        };
        if ids.iter().filter_map(crate::devices::id).any(|id| id == "PNP0A03" || id == "PNP0A08") {
            return true;
        }
    }
    false
}

/// Table 6.67 over the namespace: each device examined has its `_STA` read,
/// and each one present whose `_INI` was defined at `since` or after has it
/// run. A device at or below which no `_INI` was defined since is not
/// examined.
fn walk<K: Kernel>(aml: &mut Aml<'_, K>, names: &Names, since: u32, census: &mut Census) {
    // The devices that own an `_INI` to run, and every scope above one.
    let ini: BTreeSet<&str> =
        names.objects.iter().filter(|named| named.serial >= since && named.path.ends_with("._INI") && named.kind == Kind::Method).map(|named| parent(&named.path)).collect();
    let wanted: BTreeSet<&str> = ini.iter().flat_map(|scope| std::iter::successors(Some(*scope), |path| (*path != "\\").then(|| parent(path)))).collect();
    // The depth below which nothing is examined, where a device said so.
    let mut closed: Option<u32> = None;
    for named in &names.objects {
        if closed.is_some_and(|depth| named.depth > depth) {
            continue;
        }
        closed = None;
        if !is_device(named.kind) || !wanted.contains(named.path.as_str()) {
            continue;
        }
        if aml.host.stopping {
            return;
        }
        let status = match names.has(&named.path, "_STA") {
            false => PRESENT | FUNCTIONAL,
            true => {
                let path = format!("{}._STA", named.path);
                match evaluate(aml, census, "_STA", &path, &[]) {
                    Some(Value::Integer(status)) => {
                        census.sta += 1;
                        println!("{OWN}{path} {status:#x}");
                        status
                    }
                    Some(_) => {
                        census.refuse("_STA", &path, &toyos_aml::Error::Type("a _STA that is no Integer (§6.3.7)"));
                        continue;
                    }
                    None => continue,
                }
            }
        };
        if status & (PRESENT | FUNCTIONAL) == 0 {
            closed = Some(named.depth);
            continue;
        }
        if status & PRESENT != 0 && ini.contains(named.path.as_str()) {
            let path = format!("{}._INI", named.path);
            if evaluate(aml, census, "_INI", &path, &[]).is_some() {
                census.ini += 1;
            }
        }
    }
}

/// The interpreter's own test encodings of §20.2.
#[cfg(test)]
#[path = "../aml/tests/common/mod.rs"]
mod common;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aml::tests::{crafted, sealed, CRAFTED_RSDP};
    use crate::host::tests::Scripted;
    use toyos_abi::acpi::Space;

    use super::common::{add, arg, cat, def_name, device, field, int, method, name, op_region, ret, scope, store, string, unit};

    /// The POST port as a word field, `\P80`, which each method below writes
    /// its own tag to: what the kernel's stand-in was asked to write there
    /// is the order they ran in.
    fn post() -> Vec<u8> {
        // SystemIO; WordAcc (§20.2.5.2).
        cat(&[&op_region("P80R", 0x01, &int(0x80), &int(2)), &field("P80R", 0x02, &[unit("P80", 16)])])
    }

    fn tagged(tag: u64) -> Vec<u8> {
        store(&int(tag), &name("\\P80"))
    }

    fn posted(kernel: &Scripted) -> Vec<u64> {
        kernel.asked.borrow().iter().filter(|access| access.write == 1 && access.space == Space::SystemIo as u8 && access.address == 0x80).map(|access| access.value).collect()
    }

    /// The namespace of `dsdt` loaded and initialised, and what that ran.
    fn initialised(dsdt: &[u8]) -> (Scripted, Census) {
        let mut kernel = crafted(&sealed(b"DSDT", &cat(&[&post(), dsdt])), &[]);
        kernel.ports = vec![(0x80, 0)];
        let (_, aml) = crate::aml::load(&kernel, CRAFTED_RSDP);
        let mut aml = aml.expect("the DSDT loads");
        let names = Names::read(&aml.interpreter).expect("the namespace reads");
        let mut census = Census::default();
        initialise(&mut aml, &names, &mut census);
        drop(aml);
        (kernel, census)
    }

    /// §6.5.1: `\_SB._INI` first, then each device parent before child by
    /// Table 6.67: no `_STA` is present and functional; functional and not
    /// present runs no `_INI` and examines the children; neither examines
    /// nothing below. Each `_STA` asked is counted, and each `_INI` run.
    #[test]
    fn sb_ini_runs_first_then_each_device_by_table_6_67() {
        let dsdt = scope(
            "\\_SB",
            &cat(&[
                &method("_INI", 0, &tagged(1)),
                &device(
                    "DEVA",
                    &cat(&[
                        &method("_INI", 0, &tagged(2)),
                        &device("DEVB", &cat(&[&method("_STA", 0, &ret(&int(0x08))), &method("_INI", 0, &tagged(0xBB)), &device("DEVC", &method("_INI", 0, &tagged(3)))])),
                        &device("DEVD", &cat(&[&def_name("_STA", &int(0)), &method("_INI", 0, &tagged(0xDD)), &device("DEVE", &method("_INI", 0, &tagged(0xEE)))])),
                    ]),
                ),
                // Sleep (5) before its tag: the longest wait the census reads.
                &device("DEVF", &cat(&[&def_name("_STA", &int(0x0F)), &method("_INI", 0, &cat(&[&[0x5B, 0x22, 0x0A, 0x05], &tagged(4)]))])),
            ]),
        );
        let (kernel, census) = initialised(&dsdt);
        assert_eq!(posted(&kernel), [1, 2, 3, 4]);
        assert_eq!((census.sb_ini, census.sta, census.ini), (Some(true), 3, 3));
        assert!(census.refused.is_empty(), "{}", census.refused.counts());
        assert_eq!(
            census.line(),
            "acpiserver: the namespace initialised: \\_SB._INI run; 3 _STA read, 3 _INI run, no _REG run; the longest wait an evaluation asked 5000 µs; evaluations refused, by kind: none"
        );
    }

    /// A refused `_INI` is counted by kind and the walk goes on; a `_STA`
    /// that is refused or no Integer is counted, its device's `_INI` is not
    /// run, and its children are examined.
    #[test]
    fn a_refused_ini_or_sta_is_counted_and_the_walk_goes_on() {
        let dsdt = scope(
            "\\_SB",
            &cat(&[
                // A Divide by zero.
                &device("DEVA", &method("_INI", 0, &cat(&[&[0x78], &int(1), &int(0), &[0x00, 0x00]]))),
                &device("DEVB", &cat(&[&def_name("_STA", &string("ON")), &device("DEVC", &method("_INI", 0, &tagged(0xCC)))])),
                &device(
                    "DEVD",
                    &cat(&[&method("_STA", 0, &ret(&name("NONE"))), &method("_INI", 0, &tagged(0xDD)), &device("DEVF", &method("_INI", 0, &tagged(0xDF)))]),
                ),
                &device("DEVE", &method("_INI", 0, &tagged(5))),
            ]),
        );
        let (kernel, census) = initialised(&dsdt);
        assert_eq!(posted(&kernel), [0xCC, 0xDF, 5]);
        assert_eq!((census.sb_ini, census.sta, census.ini), (None, 0, 3));
        assert_eq!(
            census.refused.counts(),
            "_INI: Divide by zero (§19.6.32) x1; _STA: a name that does not resolve x1; _STA: a type its operator refuses: a _STA that is no Integer (§6.3.7) x1"
        );
    }

    /// A register the kernel keeps for itself is not written, and the
    /// evaluation goes on: as the T14's `HKEY._INI` writes PL1's two bytes at
    /// MCHBAR + 59A0h and then records its mode in NVS, this `_INI` runs and
    /// its record is made, and the register is said once. Every other
    /// refusal still denies the evaluation: an `_INI` that writes a page the
    /// kernel drives is refused, and what follows its write is not made.
    #[test]
    fn a_write_the_kernel_keeps_is_not_made_and_the_evaluation_goes_on() {
        const PL1: u64 = 0xFEDC_59A0;
        const DRIVEN: u64 = 0xFED9_0000;
        const NVS: u64 = 0x7700_0000;
        let dsdt = cat(&[
            &post(),
            &op_region("MCHB", 0x00, &int(PL1), &int(2)),
            // ByteAcc (§20.2.5.2).
            &field("MCHB", 0x01, &[unit("PL1L", 8), unit("PL1H", 8)]),
            &op_region("VTDR", 0x00, &int(DRIVEN), &int(1)),
            &field("VTDR", 0x01, &[unit("VTD0", 8)]),
            &op_region("NVSR", 0x00, &int(NVS), &int(2)),
            &field("NVSR", 0x01, &[unit("MODE", 8), unit("LAST", 8)]),
            &scope(
                "\\_SB",
                &cat(&[
                    &device("HKEY", &method("_INI", 0, &cat(&[&store(&int(0x5A), &name("\\PL1L")), &store(&int(0xA5), &name("\\PL1H")), &store(&int(3), &name("\\MODE"))]))),
                    &device("VTDU", &method("_INI", 0, &cat(&[&store(&int(1), &name("\\VTD0")), &store(&int(4), &name("\\LAST"))]))),
                ]),
            ),
        ]);
        let mut kernel = crafted(&sealed(b"DSDT", &dsdt), &[]);
        kernel.ports = vec![(0x80, 0)];
        kernel.memory.extend([(NVS, 10, vec![0; 2]), (PL1, toyos_abi::acpi::UNLISTED, vec![0; 8])]);
        kernel.keeps = vec![(PL1, PL1 + 8)];
        kernel.driven = vec![(DRIVEN, DRIVEN + 0x1000)];
        let (_, aml) = crate::aml::load(&kernel, CRAFTED_RSDP);
        let mut aml = aml.expect("the DSDT loads");
        let names = Names::read(&aml.interpreter).expect("the namespace reads");
        let mut census = Census::default();
        initialise(&mut aml, &names, &mut census);
        assert_eq!(census.ini, 1, "{}", census.refused.counts());
        assert_eq!(census.refused.counts(), "_INI: denied by this server: a SystemMemory write the kernel refused DeviceMemory, in unlisted firmware memory x1");
        let written = kernel.written.borrow();
        assert_eq!(written.get(&NVS), Some(&3), "the method's record after the kept write");
        assert_eq!(written.get(&(NVS + 1)), None, "a write after a refused one");
        assert!(!written.contains_key(&PL1) && !written.contains_key(&(PL1 + 1)), "a register the kernel keeps was written");
        assert_eq!(aml.host.kept.counts(), "0xfedc59a0 x2");
    }

    /// §6.5.1's walk looks for `_INI` methods: a device with no `_INI` at or below it is not
    /// examined, so a battery's `_STA` that reads the controller's space
    /// before any controller is attached is not asked, nor one that would
    /// post, and nothing is refused; a device above an `_INI` has its `_STA`
    /// asked, and the `_INI` runs.
    #[test]
    fn a_device_with_no_ini_at_or_below_it_is_not_examined() {
        let dsdt = scope(
            "\\_SB",
            &cat(&[
                &op_region("ERAM", 0x03, &int(0), &int(0xFF)),
                &field("ERAM", 0x01, &[unit("BSTA", 8)]),
                &device("BAT1", &method("_STA", 0, &ret(&name("\\_SB.BSTA")))),
                &device("LONE", &method("_STA", 0, &cat(&[&tagged(0x10), &ret(&int(0x0F))]))),
                &device(
                    "PARE",
                    &cat(&[&method("_STA", 0, &cat(&[&tagged(0x20), &ret(&int(0x0F))])), &device("KIDD", &method("_INI", 0, &tagged(0x21)))]),
                ),
            ]),
        );
        let (kernel, census) = initialised(&dsdt);
        assert_eq!(posted(&kernel), [0x20, 0x21]);
        assert_eq!((census.sta, census.ini), (1, 1));
        assert!(census.refused.is_empty(), "{}", census.refused.counts());
    }

    /// §6.5.4: a PCI_Config region on a root bus naming a `_BBN` is always
    /// available, and its scope's `_REG` is not run, the bridge's own or a
    /// device's on the bus below it; one on a root bus that names none, a
    /// host bridge by its `_HID`, and one below a bridge, are announced with
    /// `_REG(2, 1)`, before `\_SB._INI` and every device's `_INI`.
    #[test]
    fn pci_config_is_announced_before_any_ini_where_no_bbn_root_bus_holds_it() {
        let region = |n: &str| cat(&[&op_region(n, 0x02, &int(0), &int(4)), &field(n, 0x01, &[unit(&format!("{}F", &n[..3]), 8)])]);
        // `_REG` writes its tag plus Arg0 plus Arg1 shifted, so 2, 1 adds 0x12.
        let reg = |tag: u64| method("_REG", 2, &store(&add(&add(&int(tag), &arg(0), &[0x00]), &cat(&[&[0x79], &arg(1), &int(4), &[0x00]]), &[0x00]), &name("\\P80")));
        let dsdt = scope(
            "\\_SB",
            &cat(&[
                &method("_INI", 0, &tagged(0x5B)),
                &device(
                    "PCI0",
                    &cat(&[
                        &def_name("_HID", &int(0x080A_D041)),
                        &region("CFG0"),
                        &reg(0x100),
                        &device("DEV1", &cat(&[&def_name("_ADR", &int(0x0001_0000)), &region("CFG1"), &reg(0x200)])),
                    ]),
                ),
                &device(
                    "PCI1",
                    &cat(&[
                        &def_name("_BBN", &int(0x10)),
                        &region("CFGB"),
                        &reg(0xB00),
                        &method("_INI", 0, &tagged(0x99)),
                        &device("DEV2", &cat(&[&def_name("_ADR", &int(0x0002_0000)), &region("CFG2"), &reg(0xC00)])),
                        &device(
                            "BRG0",
                            &cat(&[&def_name("_ADR", &int(0x0003_0000)), &device("DEV3", &cat(&[&def_name("_ADR", &int(0)), &region("CFG3"), &reg(0x300)]))]),
                        ),
                    ]),
                ),
            ]),
        );
        let (kernel, census) = initialised(&dsdt);
        assert_eq!(posted(&kernel), [0x112, 0x212, 0x312, 0x5B, 0x99]);
        assert_eq!(census.reg, BTreeMap::from([(2, 3)]));
        assert!(census.refused.is_empty(), "{}", census.refused.counts());
        assert!(kernel.asked.borrow().iter().all(|access| access.space != Space::PciConfig as u8), "a region was read");
    }
}
