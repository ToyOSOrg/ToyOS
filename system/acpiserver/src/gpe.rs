//! The machine's general-purpose events (ACPI 6.5 §5.6.4): which GPE0 inputs
//! this server enables, by the methods `\_GPE` names and the wake events
//! devices' `_PRW` name.
//!
//! **A runtime GPE is one with a `\_GPE._Lxx` or `_Exx`** (§5.6.4.1.1), `xx`
//! its number in GPE0 in two hex digits, an `_L` a level's and an `_E` an
//! edge's, **and no wake event**: §5.6.4.2 keeps wake and runtime events off
//! one input except for the power, lid and sleep buttons and the PCI host
//! bridge's PME, so a GPE the `_PRW` of any other device names is a wake
//! event, which OSPM enables only to arm a device's wake (§5.6.10), and this
//! server arms none. A GPE that names both methods, or that is the embedded
//! controller's, which its driver serves (§5.6.4.1.2), is no runtime GPE
//! either. A `_PRW` that names a GPE block device names no GPE0 input.
//!
//! What is found is said in one line, a count each; the machine's own
//! numbers and paths go under [`OWN`].

use std::collections::{BTreeMap, BTreeSet};

use toyos_abi::acpi::Block;
use toyos_aml::{Kind, Value};

use crate::aml::{kind, Aml};
use crate::host::{Kernel, OWN};
use crate::init::Names;
use crate::sci::Trigger;

/// The devices §5.6.4.2 lets share one GPE between wake and runtime events:
/// the power, lid and sleep buttons, and the PCI host bridge.
const SHARED: [&str; 4] = ["PNP0C0C", "PNP0C0D", "PNP0C0E", "PNP0A03"];

/// The runtime GPEs of GPE0 `gpe0`, but the controller's `ec_gpe`, each with
/// its method's trigger.
pub fn runtime<K: Kernel>(aml: &mut Aml<'_, K>, names: &Names, gpe0: Block, ec_gpe: Option<u16>) -> BTreeMap<u16, Trigger> {
    // §5.2.9: a GPE block's status half holds eight GPEs a byte; its length is the FADT's byte.
    let inputs = gpe0.len / 2 * 8;
    let mut methods: BTreeMap<u16, Vec<Trigger>> = BTreeMap::new();
    for named in names.objects.iter().filter(|named| named.kind == Kind::Method) {
        let Some(name) = named.path.strip_prefix("\\_GPE.") else { continue };
        let (trigger, digits) = match name.as_bytes() {
            [b'_', b'L', ..] => (Trigger::Level, &name[2..]),
            [b'_', b'E', ..] => (Trigger::Edge, &name[2..]),
            _ => continue,
        };
        if let Ok(n) = u16::from_str_radix(digits, 16) {
            methods.entry(n).or_default().push(trigger);
        }
    }
    let mut wake = BTreeSet::new();
    for named in names.objects.iter().filter(|named| matches!(named.kind, Kind::Device | Kind::Processor { .. } | Kind::ThermalZone | Kind::PowerResource)) {
        let prw = format!("{}._PRW", named.path);
        if !names.paths.contains(&prw) {
            continue;
        }
        let n = match aml.interpreter.evaluate(&mut aml.host, &prw, &[]) {
            Ok(Value::Package(p)) => match p.first() {
                Some(&Value::Integer(n)) => n,
                // A GPE block device's input (§7.3.13), no GPE0 one.
                Some(Value::Package(_)) => continue,
                _ => {
                    println!("{OWN}{prw} names its GPE by no Integer or package");
                    continue;
                }
            },
            Ok(_) => {
                println!("{OWN}{prw} is no package");
                continue;
            }
            Err(why) => {
                println!("{OWN}{prw} was refused {why:x?}");
                println!("acpiserver: a device's _PRW was refused, so the GPE it names is not known for a wake event: {}", kind(&why));
                continue;
            }
        };
        if !shares(aml, names, &named.path) {
            wake.insert(n);
        }
    }
    let mut runtime = BTreeMap::new();
    let (mut beyond, mut both) = (0, 0);
    for (n, triggers) in &methods {
        match (triggers.as_slice(), *n < inputs) {
            (_, false) => beyond += 1,
            ([_, _, ..], true) => {
                both += 1;
                println!("{OWN}GPE {n:#x} names both an _L and an _E method");
            }
            (&[trigger], true) if Some(*n) != ec_gpe && !wake.contains(&u64::from(*n)) => {
                runtime.insert(*n, trigger);
            }
            _ => {}
        }
    }
    println!(
        "acpiserver: GPEs: {} with a method in GPE0, {} of them runtime and enabled, {} wake events left disabled, {} past GPE0, {} naming both triggers",
        methods.len() - beyond,
        runtime.len(),
        wake.len(),
        beyond,
        both
    );
    println!("{OWN}the runtime GPEs are {:x?}", runtime.keys().collect::<Vec<_>>());
    runtime
}

/// Whether the device at `path` is one §5.6.4.2 lets share a GPE between
/// wake and runtime events, by its `_HID` or a `_CID`.
fn shares<K: Kernel>(aml: &mut Aml<'_, K>, names: &Names, path: &str) -> bool {
    ["_HID", "_CID"].into_iter().any(|id| {
        let at = format!("{path}.{id}");
        if !names.paths.contains(&at) {
            return false;
        }
        let ids = match aml.interpreter.evaluate(&mut aml.host, &at, &[]) {
            Ok(Value::Package(ids)) => ids,
            Ok(id) => vec![id],
            Err(_) => return false,
        };
        ids.iter().filter_map(crate::devices::id).any(|id| SHARED.contains(&id.as_str()))
    })
}
