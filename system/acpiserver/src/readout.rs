//! Measurement scout, never landed: the Wi-Fi design's S1 read-out of the
//! namespace's PCI devices, asked for by [`ARG`]. Every host bridge (a `_HID`
//! of PNP0A08 or PNP0A03), every device directly under one that has an `_ADR`,
//! every device directly under those that has one, and every name below each
//! of the last: a function behind a root port keeps its methods there. Each
//! `_ADR` is evaluated (ACPI 6.5 §6.1.1) and nothing else is. The paths and
//! names are the firmware's choice, so their lines are under [`OWN`].

use std::collections::BTreeSet;

use toyos_aml::{Host, Interpreter, Kind, Value};

use crate::aml::kind;
use crate::devices::id;
use crate::host::OWN;

/// The argument a row gives this server to have it read out.
pub const ARG: &str = "--pci-readout";

const HOST_BRIDGES: [&str; 2] = ["PNP0A08", "PNP0A03"];

pub fn pci(interpreter: &mut Interpreter, host: &mut dyn Host) {
    let mut names: Vec<(String, Kind)> = Vec::new();
    match interpreter.walk() {
        Ok(mut walk) => {
            while let Some(entry) = walk.next() {
                names.push((walk.path().to_string(), entry.kind));
            }
        }
        Err(why) => {
            println!("acpiserver: acpi-readout: the namespace's walk was refused, {}", kind(&why));
            return;
        }
    }
    let named: BTreeSet<&str> = names.iter().map(|(path, _)| path.as_str()).collect();
    let devices: Vec<&str> =
        names.iter().filter(|(_, kind)| *kind == Kind::Device).map(|(path, _)| path.as_str()).collect();
    let parent = |path: &str| path.rsplit_once('.').map(|(above, _)| above.to_string());
    let has = |device: &str, name: &str| named.contains(format!("{device}.{name}").as_str());

    let mut bridges: Vec<&str> = Vec::new();
    for &device in devices.iter().filter(|device| has(device, "_HID")) {
        let hid = interpreter.evaluate(host, &format!("{device}._HID"), &[]);
        if hid.ok().as_ref().and_then(id).is_some_and(|hid| HOST_BRIDGES.contains(&hid.as_str())) {
            println!("{OWN}acpi-readout: host bridge {device}");
            bridges.push(device);
        }
    }
    let under = |above: &[&str]| -> Vec<&str> {
        devices
            .iter()
            .copied()
            .filter(|device| has(device, "_ADR") && parent(device).is_some_and(|p| above.contains(&p.as_str())))
            .collect()
    };
    let first = under(&bridges);
    let second = under(&first);
    for &device in first.iter().chain(&second) {
        let adr = match interpreter.evaluate(host, &format!("{device}._ADR"), &[]) {
            Ok(Value::Integer(adr)) => format!("{adr:#010x}"),
            Ok(_) => String::from("no integer"),
            Err(why) => format!("refused, {}", kind(&why)),
        };
        println!("{OWN}acpi-readout: device {device} _ADR={adr}");
    }
    let mut below = 0usize;
    for &device in &second {
        let prefix = format!("{device}.");
        for (path, kind) in names.iter().filter(|(path, _)| path.starts_with(&prefix)) {
            println!("{OWN}acpi-readout: name {path} {kind:?}");
            below += 1;
        }
    }
    println!(
        "acpiserver: acpi-readout: {} host bridge(s), {} device(s) with an _ADR under them, {} \
         under those, {below} name(s) below those",
        bridges.len(),
        first.len(),
        second.len(),
    );
}
