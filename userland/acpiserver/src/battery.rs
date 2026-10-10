//! The machine's batteries and AC adapters, as its AML reports them (ACPI 6.5
//! §10.2, §10.3), read in the namespace the load kept ([`crate::aml::Aml`]).
//!
//! **Once, after the load** ([`Power::find`]). Where the machine's row names
//! an embedded controller, each controller device (`PNP0C09`) is asked its
//! `_GLK` (§6.5.7) and then told by its `_REG` that its space is there
//! (§6.5.4, `_REG(3, 1)`), before anything reads that space: firmware
//! commonly chooses between the controller and another path by what its
//! `_REG` stored. A controller whose `_GLK` asks for the Global Lock has no
//! battery read at all: this server takes the lock around no transaction
//! yet. Then every device whose `_HID` names a control-method battery
//! (`PNP0C0A`) or an AC adapter (`ACPI0003`) is found by a walk, a battery
//! is kept where its `_STA` says one is present (§6.3.7, bit 4; a device
//! with no `_STA` has none), and its static information is read once, from
//! `_BIX` where it has one and `_BIF` where it has not (§10.2.2).
//!
//! **Every [`POLL`]** each battery's `_BST` and each adapter's `_PSR` are
//! evaluated ([`Power::read`]), and a line is said on the first reading and
//! wherever a battery's whole percent or state, or the adapter's, moved since
//! the line before: a draining battery says a line a percent, never one a
//! poll.
//!
//! **What a line says is numbers.** The model, serial number, type and OEM
//! strings of `_BIX` and `_BIF` are never taken out of the package, and a
//! device's path is the firmware's choice, said only under [`OWN`]. A
//! refused evaluation is said under [`REFUSED`] the first time and counted
//! after; its battery or adapter reads as unknown until one answers.

use std::fmt;
use std::time::Duration;

use toyos_aml::{Error, Kind, Value};

use crate::aml::{kind, Aml};
use crate::host::{Controller, Kernel, OWN};
use crate::ledger::Ledger;

/// How often each battery and adapter is read.
pub const POLL: Duration = Duration::from_secs(10);

/// What opens the line of an evaluation this module asked for and the
/// interpreter or the host refused.
pub const REFUSED: &str = "acpiserver: a method read for the power sources refused: ";

/// §5.6.7 / §6.1.5 identifiers, compared in their text form.
const CONTROLLER: &str = "PNP0C09";
const BATTERY: &str = "PNP0C0A";
const ADAPTER: &str = "ACPI0003";

/// §10.2.2: a capacity, rate or voltage the battery does not know.
const UNKNOWN: u64 = 0xFFFF_FFFF;

/// `_BST`'s Battery State bits (§10.2.2, "_BST").
const DISCHARGING: u64 = 1 << 0;
const CHARGING: u64 = 1 << 1;
const CRITICAL: u64 = 1 << 2;
const LIMITING: u64 = 1 << 3;

/// `_STA`'s battery-present bit (§6.3.7).
const BATTERY_PRESENT: u64 = 1 << 4;

/// What `_BIF` and `_BIX` say a battery's capacities and rates are counted
/// in (their Power Unit).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Unit {
    /// Capacities in mWh, rates in mW.
    Power,
    /// Capacities in mAh, rates in mA.
    Current,
}

impl Unit {
    fn capacity(self) -> &'static str {
        match self {
            Unit::Power => "mWh",
            Unit::Current => "mAh",
        }
    }

    fn rate(self) -> &'static str {
        match self {
            Unit::Power => "mW",
            Unit::Current => "mA",
        }
    }
}

/// A battery's static information, the numbers of `_BIX` or `_BIF`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Info {
    /// Which of the two it came from.
    pub from: &'static str,
    pub unit: Unit,
    pub design: Option<u32>,
    pub full: Option<u32>,
    /// mV.
    pub design_voltage: Option<u32>,
    /// `_BIX`'s alone.
    pub cycles: Option<u32>,
}

/// One `_BST`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Status {
    /// The Battery State bits as the firmware returned them.
    pub state: u64,
    /// In the battery's [`Unit`]'s rate.
    pub rate: Option<u32>,
    /// In its capacity.
    pub remaining: Option<u32>,
    /// mV.
    pub voltage: Option<u32>,
}

/// One element of a package of DWORDs: an Integer no wider than 32 bits,
/// [`UNKNOWN`] read as `None`.
fn dword(elements: &[Value], at: usize, what: &str) -> Result<Option<u32>, String> {
    match elements.get(at) {
        Some(&Value::Integer(UNKNOWN)) => Ok(None),
        Some(&Value::Integer(value)) => u32::try_from(value).map(Some).map_err(|_| format!("its {what} is wider than a DWORD")),
        Some(_) => Err(format!("its {what} is no Integer")),
        None => Err(format!("it has no {what}")),
    }
}

fn package<'v>(value: &'v Value, least: usize) -> Result<&'v [Value], String> {
    match value {
        Value::Package(elements) if elements.len() >= least => Ok(elements),
        Value::Package(elements) => Err(format!("a package of {} elements, where §10.2.2 has at least {least}", elements.len())),
        _ => Err("no package".into()),
    }
}

fn unit(elements: &[Value], at: usize) -> Result<Unit, String> {
    match elements.get(at) {
        Some(Value::Integer(0)) => Ok(Unit::Power),
        Some(Value::Integer(1)) => Ok(Unit::Current),
        Some(Value::Integer(other)) => Err(format!("its Power Unit is {other}, which §10.2.2 does not define")),
        _ => Err("its Power Unit is no Integer".into()),
    }
}

/// `_BIF`: Power Unit, Design Capacity, Last Full Charge Capacity, Battery
/// Technology, Design Voltage, and eight more this server reads nothing of.
pub fn bif(value: &Value) -> Result<Info, String> {
    let elements = package(value, 13)?;
    Ok(Info {
        from: "_BIF",
        unit: unit(elements, 0)?,
        design: dword(elements, 1, "Design Capacity")?,
        full: dword(elements, 2, "Last Full Charge Capacity")?,
        design_voltage: dword(elements, 4, "Design Voltage")?,
        cycles: None,
    })
}

/// `_BIX`: `_BIF`'s numbers behind a Revision, with the Cycle Count after
/// them. Revision 0 has 20 elements and revision 1 adds a 21st; the fields
/// read here are at the same places in both.
pub fn bix(value: &Value) -> Result<Info, String> {
    let elements = package(value, 20)?;
    Ok(Info {
        from: "_BIX",
        unit: unit(elements, 1)?,
        design: dword(elements, 2, "Design Capacity")?,
        full: dword(elements, 3, "Last Full Charge Capacity")?,
        design_voltage: dword(elements, 5, "Design Voltage")?,
        cycles: dword(elements, 8, "Cycle Count")?,
    })
}

/// `_BST`: Battery State, Present Rate, Remaining Capacity, Present Voltage.
pub fn bst(value: &Value) -> Result<Status, String> {
    let elements = package(value, 4)?;
    let state = match elements[0] {
        Value::Integer(state) if state & (CHARGING | DISCHARGING) == CHARGING | DISCHARGING => {
            return Err("its state says charging and discharging at once, which §10.2.2 forbids".into());
        }
        Value::Integer(state) => state,
        _ => return Err("its Battery State is no Integer".into()),
    };
    Ok(Status {
        state,
        rate: dword(elements, 1, "Battery Present Rate")?,
        remaining: dword(elements, 2, "Battery Remaining Capacity")?,
        voltage: dword(elements, 3, "Battery Present Voltage")?,
    })
}

/// The charge in whole percent of the last full charge, rounded down.
pub fn percent(info: &Info, status: &Status) -> Option<u64> {
    let full = info.full.filter(|&full| full > 0)?;
    Some(u64::from(status.remaining?) * 100 / u64::from(full))
}

/// The power the battery moves, in mW: its rate where that is power, and
/// the rate times its present voltage where that is current.
pub fn milliwatts(info: &Info, status: &Status) -> Option<u64> {
    let rate = u64::from(status.rate?);
    match info.unit {
        Unit::Power => Some(rate),
        Unit::Current => Some(rate * u64::from(status.voltage?) / 1000),
    }
}

/// A battery state's bits by name.
fn state(bits: u64) -> String {
    let mut said: Vec<String> = [(DISCHARGING, "discharging"), (CHARGING, "charging"), (CRITICAL, "critical"), (LIMITING, "charge limiting")]
        .iter()
        .filter(|&&(bit, _)| bits & bit != 0)
        .map(|&(_, name)| name.into())
        .collect();
    let reserved = bits & !(DISCHARGING | CHARGING | CRITICAL | LIMITING);
    if reserved != 0 {
        said.push(format!("reserved bits {reserved:#x}"));
    }
    if said.is_empty() { "neither charging nor discharging".into() } else { said.join(", ") }
}

struct Known(Option<u32>, &'static str);

impl fmt::Display for Known {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(value) => write!(f, "{value} {}", self.1),
            None => f.write_str("unknown"),
        }
    }
}

/// The information line's numbers.
pub fn said_info(info: &Info) -> String {
    let unit = info.unit.capacity();
    format!(
        "from {}: design {}, last full {}, design voltage {}, {}",
        info.from,
        Known(info.design, unit),
        Known(info.full, unit),
        Known(info.design_voltage, "mV"),
        match info.cycles {
            Some(cycles) => format!("{cycles} cycles"),
            None => "cycles unknown".into(),
        }
    )
}

/// A reading line's numbers.
pub fn said_status(info: &Info, status: &Status) -> String {
    format!(
        "{}, {} of {}, {}, rate {}, {}, moving {}",
        percent(info, status).map_or("charge unknown".into(), |percent| format!("{percent}%")),
        Known(status.remaining, info.unit.capacity()),
        Known(info.full, info.unit.capacity()),
        state(status.state),
        Known(status.rate, info.unit.rate()),
        Known(status.voltage, "mV"),
        milliwatts(info, status).map_or("unknown mW".into(), |mw| format!("{mw} mW")),
    )
}

/// An `_HID`'s text: a string as it is, an integer as the EISA ID it
/// compresses (§6.1.5): three letters of five bits each and four hex digits,
/// stored in the byte order of their text.
fn hid(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => String::from_utf8(text.clone()).ok(),
        &Value::Integer(id) => {
            let id = u32::try_from(id).ok()?.swap_bytes();
            if id >> 31 != 0 {
                return None;
            }
            let letter = |at: u32| char::from(b'@' + ((id >> at) & 0x1F) as u8);
            Some(format!("{}{}{}{:04X}", letter(26), letter(21), letter(16), id & 0xFFFF))
        }
        _ => None,
    }
}

struct Battery {
    path: String,
    info: Info,
    /// What its last line said: whole percent and state.
    said: Option<(Option<u64>, u64)>,
}

/// The machine's power sources, found once and read every [`POLL`].
pub struct Power {
    batteries: Vec<Battery>,
    adapters: Vec<String>,
    /// What the last line said of the adapters.
    ac_said: Option<Option<bool>>,
    refused: Ledger,
    /// The last evaluation found the machine stopping.
    stopping: bool,
}

impl Power {
    /// An evaluation of `path` with `args`: `Ok(None)` where nothing is
    /// named there.
    fn evaluate<K: Kernel, C: Controller>(&mut self, aml: &mut Aml<'_, K, C>, path: &str, args: &[Value]) -> Result<Option<Value>, Error> {
        let answer = match aml.interpreter.evaluate(&mut aml.host, path, args) {
            Err(Error::NotFound(named)) if named == path => Ok(None),
            answer => answer.map(Some),
        };
        self.stopping = aml.host.stopping;
        answer
    }

    /// A refusal of `method` of the device at `device`, said the first time;
    /// on a stopping machine every evaluation is refused, and none is said.
    fn refused(&mut self, device: &str, method: &str, why: &str) {
        if self.stopping {
            return;
        }
        let what = format!("{method}: {why}");
        if self.refused.see(&what) {
            println!("{REFUSED}{what}");
            println!("{OWN}that was {device}.{method}");
        }
    }

    /// Find the machine's batteries and adapters in `aml`, telling its
    /// controller its space is there first, and read each battery's
    /// information; `None` where there is nothing to read.
    pub fn find<K: Kernel, C: Controller>(aml: &mut Aml<'_, K, C>, controller: bool) -> Option<Power> {
        let mut power = Power { batteries: Vec::new(), adapters: Vec::new(), ac_said: None, refused: Ledger::default(), stopping: false };
        let (takes, contended, given_back) = (aml.host.takes, aml.host.contended, aml.host.given_back);
        let hids: Vec<String> = match aml.interpreter.walk() {
            Ok(mut walk) => {
                let mut found = Vec::new();
                while let Some(entry) = walk.next() {
                    if entry.name == *b"_HID" && entry.kind != Kind::Device {
                        found.push(walk.path().to_owned());
                    }
                }
                found
            }
            Err(why) => {
                println!("acpiserver: {}the namespace's walk was refused: {}", acpiserver_api::NO_BATTERY, kind(&why));
                return None;
            }
        };
        let mut controllers = Vec::new();
        let mut batteries = Vec::new();
        for path in hids {
            let device = path.strip_suffix("._HID").expect("the walk's path of a `_HID` ends in it").to_owned();
            let id = match power.evaluate(aml, &path, &[]) {
                Ok(value) => value.as_ref().and_then(hid),
                Err(why) => {
                    power.refused(&device, "_HID", &kind(&why));
                    None
                }
            };
            match id.as_deref() {
                Some(CONTROLLER) => controllers.push(device),
                Some(BATTERY) => batteries.push(device),
                Some(ADAPTER) => power.adapters.push(device),
                _ => {}
            }
        }
        if power.stopping {
            return None;
        }

        if controller {
            for device in &controllers {
                match power.evaluate(aml, &format!("{device}._GLK"), &[]) {
                    Ok(None | Some(Value::Integer(0))) => {}
                    Ok(Some(_)) => {
                        println!(
                            "acpiserver: {}the embedded controller's _GLK asks for the Global Lock around its transactions, which this server takes around none yet",
                            acpiserver_api::NO_BATTERY
                        );
                        println!("{OWN}that was {device}._GLK");
                        return None;
                    }
                    Err(why) => power.refused(device, "_GLK", &kind(&why)),
                }
                // §6.5.4: Arg0 the EmbeddedControl space, Arg1 1 for "connect".
                if let Err(why) = power.evaluate(aml, &format!("{device}._REG"), &[Value::Integer(3), Value::Integer(1)]) {
                    power.refused(device, "_REG", &kind(&why));
                }
            }
        }

        let count = batteries.len();
        for device in batteries {
            let sta = match power.evaluate(aml, &format!("{device}._STA"), &[]) {
                Ok(None) => 0x0F,
                Ok(Some(Value::Integer(sta))) => sta,
                Ok(Some(_)) => {
                    power.refused(&device, "_STA", "no Integer");
                    continue;
                }
                Err(why) => {
                    power.refused(&device, "_STA", &kind(&why));
                    continue;
                }
            };
            if sta & BATTERY_PRESENT == 0 {
                println!("{OWN}{device} says no battery is present: _STA {sta:#x}");
                continue;
            }
            let info = match power.evaluate(aml, &format!("{device}._BIX"), &[]) {
                Ok(None) => match power.evaluate(aml, &format!("{device}._BIF"), &[]) {
                    Ok(None) => Err(("_BIF", "the device has neither _BIX nor _BIF".to_owned())),
                    Ok(Some(value)) => bif(&value).map_err(|why| ("_BIF", why)),
                    Err(why) => Err(("_BIF", kind(&why))),
                },
                Ok(Some(value)) => bix(&value).map_err(|why| ("_BIX", why)),
                Err(why) => Err(("_BIX", kind(&why))),
            };
            match info {
                Ok(info) => power.batteries.push(Battery { path: device, info, said: None }),
                Err((method, why)) => power.refused(&device, method, &why),
            }
        }
        if power.stopping {
            return None;
        }

        let present = power.batteries.len();
        for (n, battery) in power.batteries.iter().enumerate() {
            println!("acpiserver: {}battery {} of {present} {}", acpiserver_api::BATTERY_INFO, n + 1, said_info(&battery.info));
            println!("{OWN}battery {} of {present} is {}", n + 1, battery.path);
        }
        if present > 0 {
            for line in power.read(aml) {
                println!("{line}");
            }
        }
        let found = format!(
            "{present} of {count} control-method batteries present, {} AC adapter(s), {} embedded controller device(s) told their space is there; the Global Lock taken {} times on the way and given back {} times, the firmware found holding it by {} of the takes",
            power.adapters.len(),
            if controller { controllers.len() } else { 0 },
            aml.host.takes - takes,
            aml.host.given_back - given_back,
            aml.host.contended - contended,
        );
        if present == 0 {
            println!("acpiserver: {}{found}", acpiserver_api::NO_BATTERY);
            return None;
        }
        println!("acpiserver: power sources: {found}, the first reading's included; each read every {} s", POLL.as_secs());
        Some(power)
    }

    /// Read every battery and adapter: the line of each battery whose
    /// reading moved, for its caller to say.
    pub fn read<K: Kernel, C: Controller>(&mut self, aml: &mut Aml<'_, K, C>) -> Vec<String> {
        let mut lines = Vec::new();
        let mut ac = None;
        for at in 0..self.adapters.len() {
            let device = self.adapters[at].clone();
            match self.evaluate(aml, &format!("{device}._PSR"), &[]) {
                Ok(Some(Value::Integer(online))) => ac = Some(ac.unwrap_or(false) || online != 0),
                Ok(Some(_)) => self.refused(&device, "_PSR", "no Integer"),
                Ok(None) => self.refused(&device, "_PSR", "the adapter has no _PSR"),
                Err(_) if self.stopping => return lines,
                Err(why) => self.refused(&device, "_PSR", &kind(&why)),
            }
        }
        let ac_moved = self.ac_said != Some(ac);
        self.ac_said = Some(ac);
        let ac = match ac {
            Some(true) => "AC online",
            Some(false) => "AC offline",
            None if self.adapters.is_empty() => "no AC adapter",
            None => "AC unknown",
        };
        let count = self.batteries.len();
        for n in 0..count {
            let device = self.batteries[n].path.clone();
            let status = match self.evaluate(aml, &format!("{device}._BST"), &[]) {
                Ok(Some(value)) => bst(&value),
                Ok(None) => Err("the battery has no _BST".into()),
                Err(_) if self.stopping => return lines,
                Err(why) => Err(kind(&why)),
            };
            let status = match status {
                Ok(status) => status,
                Err(why) => {
                    self.refused(&device, "_BST", &why);
                    continue;
                }
            };
            let battery = &mut self.batteries[n];
            let now = (percent(&battery.info, &status), status.state);
            if battery.said != Some(now) || ac_moved {
                battery.said = Some(now);
                lines.push(format!("acpiserver: {}battery {} of {count}: {}; {ac}", acpiserver_api::BATTERY_READ, n + 1, said_status(&battery.info, &status)));
            }
        }
        lines
    }
}

/// The interpreter's own test encodings of §20.2, which the namespace
/// tests below assemble a machine's tables from.
#[cfg(test)]
#[path = "../aml/tests/common/mod.rs"]
mod common;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aml::tests::{crafted, machine, sealed, CRAFTED_RSDP, QEMU, RSDP};
    use crate::ec::tests::Emulated;
    use crate::host::tests::Scripted;

    use super::common;
    use common::{device, field, if_, index, int, lequal, method, op_region, package, ret, scope, skip, store, string, unit as field_unit};

    /// The controller's space as the fixture's fields lay it out.
    const STATE: u8 = 0x38;
    const AC: u8 = 0x46;
    const PAGE: u8 = 0x81;
    const RATE: u8 = 0xA0;
    const VOLTAGE: u8 = 0xA8;

    /// A laptop's namespace as §10.2.2, §10.3 and §6.5.4 have one: an
    /// embedded controller whose `_REG` records that its space is there; a
    /// battery below it whose `_STA` says it is present only once `_REG` has
    /// run, whose `_BIX` is constant and whose `_BST` selects a page of the
    /// controller's space with a write and then reads it; and an adapter whose
    /// `_PSR` reads the controller. `glk` is the controller's `_GLK`, where
    /// it has one.
    fn laptop(glk: Option<u64>) -> Vec<u8> {
        let fields = [
            skip(8 * usize::from(STATE)),
            field_unit("BSTA", 8),
            skip(8 * usize::from(AC - STATE - 1)),
            field_unit("ACST", 8),
            skip(8 * usize::from(PAGE - AC - 1)),
            field_unit("PAGE", 8),
            skip(8 * usize::from(RATE - PAGE - 1)),
            field_unit("RATE", 16),
            field_unit("REMN", 16),
            skip(8 * usize::from(VOLTAGE - RATE - 4)),
            field_unit("VOLT", 16),
        ];
        let mut bix: Vec<Vec<u8>> = [1, 0, 57000, 50000, 1, 11520, 5700, 570, 312, 95000, 0xFFFF_FFFF, 0xFFFF_FFFF, 1000, 1000, 100, 100]
            .map(int)
            .to_vec();
        bix.extend(["MODEL", "SERIAL", "LION", "OEM"].map(string));
        bix.push(int(1));
        let bst = [
            store(b"NVSB", &[0x60]),
            store(&int(1), b"PAGE"),
            store(b"BSTA", &index(b"BSTP", &int(0), &[0])),
            store(b"RATE", &index(b"BSTP", &int(1), &[0])),
            store(b"REMN", &index(b"BSTP", &int(2), &[0])),
            store(b"VOLT", &index(b"BSTP", &int(3), &[0])),
            ret(b"BSTP"),
        ]
        .concat();
        let battery = device(
            "BAT0",
            &[
                common::def_name("_HID", &int(0x0A0C_D041)),
                method("_STA", 0, &[if_(b"ECOK", &ret(&int(0x1F))), ret(&int(0x0F))].concat()),
                method("_BIX", 0, &ret(&package(&bix))),
                common::def_name("BSTP", &package(&[int(0), int(0), int(0), int(0)])),
                method("_BST", 0, &bst),
            ]
            .concat(),
        );
        let controller = device(
            "EC0",
            &[
                common::def_name("_HID", &int(0x090C_D041)),
                op_region("NVSR", 0x00, &int(NVS), &int(1)),
                field("NVSR", 0x01, &[field_unit("NVSB", 8)]),
                op_region("ECOR", 0x03, &int(0), &int(0x100)),
                field("ECOR", 0x01, &fields),
                common::def_name("ECOK", &int(0)),
                method("_REG", 2, &if_(&lequal(&[0x68], &int(3)), &store(&[0x69], b"ECOK"))),
                glk.map_or(Vec::new(), |glk| common::def_name("_GLK", &int(glk))),
                battery,
            ]
            .concat(),
        );
        let adapter = device("AC", &[common::def_name("_HID", &string("ACPI0003")), method("_PSR", 0, &ret(&common::name("\\_SB.EC0.ACST")))].concat());
        sealed(b"DSDT", &scope("\\_SB", &[controller, adapter].concat()))
    }

    /// What the fixture's controller holds: discharging at 6210 mW with
    /// 43511 of 50000 mWh left at 12312 mV, off AC.
    fn space() -> [u8; 256] {
        let mut space = [0; 256];
        space[usize::from(STATE)] = 1;
        let at = usize::from(RATE);
        space[at..at + 2].copy_from_slice(&6210u16.to_le_bytes());
        space[at + 2..at + 4].copy_from_slice(&43511u16.to_le_bytes());
        let at = usize::from(VOLTAGE);
        space[at..at + 2].copy_from_slice(&12312u16.to_le_bytes());
        space
    }

    /// A byte of firmware memory `_BST` reads first, as real ones read their
    /// NVS.
    const NVS: u64 = 0x7700_0000;

    fn laptop_machine(glk: Option<u64>) -> Scripted {
        let mut kernel = crafted(&laptop(glk), &[]);
        kernel.memory.push((NVS, 10, vec![0]));
        kernel
    }

    fn loaded(kernel: &Scripted, ec: Option<Emulated>) -> Aml<'_, Scripted, Emulated> {
        let (_, aml) = crate::aml::load(kernel, ec, CRAFTED_RSDP);
        aml.expect("the fixture's DSDT loads")
    }

    fn done(aml: &Aml<'_, Scripted, Emulated>) -> Vec<(u8, u8, u8)> {
        aml.host.ec.as_ref().expect("a controller").done.clone()
    }

    const RD: u8 = 0x80;
    const WR: u8 = 0x81;

    /// The controller is told its space is there before the battery is asked
    /// whether it is present, which here it says only after; its information
    /// is read once and the battery read with it, through the controller's
    /// transactions alone: the adapter's byte, then the page `_BST` selects,
    /// written before its fields are read, a byte at a time.
    #[test]
    fn a_laptops_battery_is_found_after_reg_and_read_through_the_controller() {
        let kernel = laptop_machine(None);
        let mut aml = loaded(&kernel, Some(Emulated::new(space())));
        let power = Power::find(&mut aml, true).expect("a battery");
        assert_eq!(power.adapters.len(), 1);
        assert_eq!(power.batteries.len(), 1);
        assert_eq!(power.batteries[0].path, "\\_SB_.EC0_.BAT0");
        assert_eq!(
            power.batteries[0].info,
            Info { from: "_BIX", unit: Unit::Power, design: Some(57000), full: Some(50000), design_voltage: Some(11520), cycles: Some(312) }
        );
        assert_eq!(
            done(&aml),
            [
                (RD, AC, 0),
                (WR, PAGE, 1),
                (RD, STATE, 0),
                (RD, RATE, 0),
                (RD, RATE + 1, 0),
                (RD, RATE + 2, 0),
                (RD, RATE + 3, 0),
                (RD, VOLTAGE, 0),
                (RD, VOLTAGE + 1, 0),
            ]
        );
        assert_eq!(power.batteries[0].said, Some((Some(87), DISCHARGING)));
        assert_eq!(power.ac_said, Some(Some(false)));
        assert!(power.refused.is_empty() && aml.host.refused.is_empty());
        assert!(kernel.asked.borrow().iter().all(|access| access.write == 0), "the kernel was asked for a write");
    }

    /// A line is said only where what it says moved: the percent, the state
    /// or the adapter; find said the first.
    #[test]
    fn a_reading_is_said_only_where_its_percent_its_state_or_the_adapter_moves() {
        let kernel = laptop_machine(None);
        let mut aml = loaded(&kernel, Some(Emulated::new(space())));
        let mut power = Power::find(&mut aml, true).expect("a battery");
        let poke = |aml: &mut Aml<'_, Scripted, Emulated>, at: u8, bytes: &[u8]| {
            let ec = aml.host.ec.as_mut().expect("a controller");
            ec.space[usize::from(at)..usize::from(at) + bytes.len()].copy_from_slice(bytes);
        };
        assert_eq!(power.read(&mut aml), Vec::<String>::new(), "nothing moved since find's reading");
        poke(&mut aml, RATE, &7000u16.to_le_bytes());
        poke(&mut aml, RATE + 2, &43600u16.to_le_bytes());
        assert_eq!(power.read(&mut aml), Vec::<String>::new(), "87% discharging at another rate is no new line");
        poke(&mut aml, RATE + 2, &43000u16.to_le_bytes());
        assert_eq!(
            power.read(&mut aml),
            ["acpiserver: battery read: battery 1 of 1: 86%, 43000 mWh of 50000 mWh, discharging, rate 7000 mW, 12312 mV, moving 7000 mW; AC offline"]
        );
        poke(&mut aml, AC, &[1]);
        assert_eq!(
            power.read(&mut aml),
            ["acpiserver: battery read: battery 1 of 1: 86%, 43000 mWh of 50000 mWh, discharging, rate 7000 mW, 12312 mV, moving 7000 mW; AC online"]
        );
        poke(&mut aml, STATE, &[CHARGING as u8]);
        assert_eq!(
            power.read(&mut aml),
            ["acpiserver: battery read: battery 1 of 1: 86%, 43000 mWh of 50000 mWh, charging, rate 7000 mW, 12312 mV, moving 7000 mW; AC online"]
        );
        assert_eq!(power.read(&mut aml), Vec::<String>::new());
    }

    /// No controller in the machine's row: `_REG` is not run, the battery
    /// says it is absent, and the controller's space is never reached.
    #[test]
    fn a_row_with_no_controller_runs_no_reg_and_reads_no_battery() {
        let kernel = laptop_machine(None);
        let mut aml = loaded(&kernel, None);
        assert!(Power::find(&mut aml, false).is_none());
        let ecok = aml.interpreter.evaluate(&mut aml.host, "\\_SB_.EC0_.ECOK", &[]);
        assert_eq!(ecok, Ok(Value::Integer(0)), "_REG ran for a controller the row does not name");
        assert!(aml.host.refused.is_empty());
    }

    /// §6.5.7: a controller whose `_GLK` is 1 wants the Global Lock around
    /// each transaction, which this server does not take: no battery is
    /// read, and the controller is not even told its space is there.
    #[test]
    fn a_controller_that_wants_the_global_lock_has_no_battery_read() {
        for (glk, read) in [(Some(1), false), (Some(0), true), (None, true)] {
            let kernel = laptop_machine(glk);
            let mut aml = loaded(&kernel, Some(Emulated::new(space())));
            assert_eq!(Power::find(&mut aml, true).is_some(), read, "_GLK {glk:?}");
            let ecok = aml.interpreter.evaluate(&mut aml.host, "\\_SB_.EC0_.ECOK", &[]);
            assert_eq!(ecok, Ok(Value::Integer(u64::from(read))), "_GLK {glk:?}");
            assert_eq!(done(&aml).is_empty(), !read, "_GLK {glk:?}: {:x?}", done(&aml));
        }
    }

    /// QEMU's q35 has neither a battery nor an adapter, and its row no
    /// controller.
    #[test]
    fn qemus_namespace_has_no_battery() {
        let kernel = machine(QEMU);
        let (_, aml) = crate::aml::load(&kernel, None::<Emulated>, RSDP);
        let mut aml = aml.expect("QEMU's DSDT loads");
        assert!(Power::find(&mut aml, false).is_none());
        assert!(aml.host.refused.is_empty());
    }

    /// A `_BST` that is no package of four is said once and counted, and the
    /// battery's last line stands.
    #[test]
    fn a_refused_reading_is_said_once_and_counted() {
        let kernel = laptop_machine(None);
        let mut aml = loaded(&kernel, Some(Emulated::new(space())));
        let mut power = Power::find(&mut aml, true).expect("a battery");
        power.read(&mut aml);
        let said = power.batteries[0].said;
        aml.host.ec.as_mut().expect("a controller").space[usize::from(STATE)] = (CHARGING | DISCHARGING) as u8;
        for _ in 0..3 {
            power.read(&mut aml);
        }
        assert_eq!(power.batteries[0].said, said);
        assert_eq!(power.refused.counts(), "_BST: its state says charging and discharging at once, which §10.2.2 forbids x3");
    }

    /// A machine that stops while `_BST` reads its firmware memory ends the
    /// read with nothing refused, and its battery's last line stands.
    #[test]
    fn a_machine_that_stops_mid_read_refuses_nothing() {
        let kernel = laptop_machine(None);
        let mut aml = loaded(&kernel, Some(Emulated::new(space())));
        let mut power = Power::find(&mut aml, true).expect("a battery");
        power.read(&mut aml);
        let said = power.batteries[0].said;
        kernel.stops_after.set(Some(0));
        power.read(&mut aml);
        assert!(aml.host.stopping, "the read reached the kernel");
        assert_eq!(power.batteries[0].said, said);
        assert!(power.refused.is_empty(), "{}", power.refused.counts());
    }

    fn integers(values: &[u64]) -> Vec<Value> {
        values.iter().map(|&value| Value::Integer(value)).collect()
    }

    /// A `_BIF` of §10.2.2's 13 elements: the numbers given, then the
    /// granularities and the four strings.
    fn bif_of(numbers: [u64; 9]) -> Value {
        let mut elements = integers(&numbers);
        elements.extend(["MODEL", "SERIAL", "LION", "OEM"].map(|s| Value::String(s.as_bytes().to_vec())));
        Value::Package(elements)
    }

    /// A `_BIX` of `len` elements, revision first: Power Unit, Design
    /// Capacity, Last Full Charge Capacity, Technology, Design Voltage,
    /// warning and low capacities, Cycle Count, then the rest.
    fn bix_of(revision: u64, numbers: [u64; 8], len: usize) -> Value {
        let mut elements = integers(&[revision]);
        elements.extend(integers(&numbers));
        elements.extend(integers(&[95000, 0xFFFF_FFFF, 0xFFFF_FFFF, 1000, 1000, 100, 100]));
        elements.extend(["MODEL", "SERIAL", "LION", "OEM"].map(|s| Value::String(s.as_bytes().to_vec())));
        if len == 21 {
            elements.push(Value::Integer(1));
        }
        assert_eq!(elements.len(), len);
        Value::Package(elements)
    }

    #[test]
    fn a_bif_is_read_by_its_place_in_the_package() {
        let info = bif(&bif_of([0, 57000, 51230, 1, 11520, 2850, 570, 100, 100])).expect("a _BIF");
        assert_eq!(
            info,
            Info { from: "_BIF", unit: Unit::Power, design: Some(57000), full: Some(51230), design_voltage: Some(11520), cycles: None }
        );
        assert_eq!(said_info(&info), "from _BIF: design 57000 mWh, last full 51230 mWh, design voltage 11520 mV, cycles unknown");
    }

    #[test]
    fn a_bix_of_either_revision_is_read_at_the_same_places() {
        for (revision, len) in [(0, 20), (1, 21)] {
            let info = bix(&bix_of(revision, [1, 4000, 3600, 1, 15400, 400, 200, 312], len)).expect("a _BIX");
            assert_eq!(
                info,
                Info { from: "_BIX", unit: Unit::Current, design: Some(4000), full: Some(3600), design_voltage: Some(15400), cycles: Some(312) }
            );
            assert_eq!(said_info(&info), "from _BIX: design 4000 mAh, last full 3600 mAh, design voltage 15400 mV, 312 cycles");
        }
    }

    /// §10.2.2: 0xFFFFFFFF is a value the battery does not know, and is said
    /// so, never as a number.
    #[test]
    fn an_unknown_dword_is_unknown_and_never_a_number() {
        let info = bix(&bix_of(1, [0, UNKNOWN, UNKNOWN, 1, UNKNOWN, 0, 0, UNKNOWN], 21)).expect("a _BIX");
        assert_eq!((info.design, info.full, info.design_voltage, info.cycles), (None, None, None, None));
        assert_eq!(said_info(&info), "from _BIX: design unknown, last full unknown, design voltage unknown, cycles unknown");
        let status = bst(&Value::Package(integers(&[DISCHARGING, UNKNOWN, UNKNOWN, UNKNOWN]))).expect("a _BST");
        assert_eq!(said_status(&info, &status), "charge unknown, unknown of unknown, discharging, rate unknown, unknown, moving unknown mW");
    }

    #[test]
    fn a_bst_reads_its_four_dwords_and_names_its_state() {
        let info = bif(&bif_of([0, 57000, 50000, 1, 11520, 0, 0, 1, 1])).expect("a _BIF");
        let status = bst(&Value::Package(integers(&[DISCHARGING, 6210, 43511, 12312]))).expect("a _BST");
        assert_eq!(status, Status { state: DISCHARGING, rate: Some(6210), remaining: Some(43511), voltage: Some(12312) });
        assert_eq!(percent(&info, &status), Some(87), "rounded down");
        assert_eq!(milliwatts(&info, &status), Some(6210), "a rate in mW is the power");
        assert_eq!(said_status(&info, &status), "87%, 43511 mWh of 50000 mWh, discharging, rate 6210 mW, 12312 mV, moving 6210 mW");

        let charging = bst(&Value::Package(integers(&[CHARGING | CRITICAL | LIMITING | 0x30, 0, 50000, 13000]))).expect("a _BST");
        assert_eq!(state(charging.state), "charging, critical, charge limiting, reserved bits 0x30");
        assert_eq!(state(0), "neither charging nor discharging");
        assert_eq!(percent(&info, &charging), Some(100));
    }

    /// A rate in mA moves the rate times the present voltage: mA × mV / 1000
    /// is mW. With no voltage known there is no power, and no design voltage
    /// stands in for it.
    #[test]
    fn a_current_is_power_only_at_its_present_voltage() {
        let info = bix(&bix_of(1, [1, 4000, 3600, 1, 15400, 0, 0, 1], 21)).expect("a _BIX");
        let status = bst(&Value::Package(integers(&[DISCHARGING, 543, 1800, 13672]))).expect("a _BST");
        assert_eq!(milliwatts(&info, &status), Some(543 * 13672 / 1000));
        assert_eq!(percent(&info, &status), Some(50));
        assert_eq!(said_status(&info, &status), "50%, 1800 mAh of 3600 mAh, discharging, rate 543 mA, 13672 mV, moving 7423 mW");
        let unknown_voltage = Status { voltage: None, ..status };
        assert_eq!(milliwatts(&info, &unknown_voltage), None);
    }

    #[test]
    fn a_full_charge_of_zero_is_no_percent() {
        let info = bif(&bif_of([0, 57000, 0, 1, 11520, 0, 0, 1, 1])).expect("a _BIF");
        let status = bst(&Value::Package(integers(&[0, 0, 100, 12000]))).expect("a _BST");
        assert_eq!(percent(&info, &status), None);
    }

    #[test]
    fn a_package_that_is_not_what_10_2_2_says_is_refused_by_name() {
        assert_eq!(bst(&Value::Package(integers(&[CHARGING | DISCHARGING, 0, 0, 0]))), Err("its state says charging and discharging at once, which §10.2.2 forbids".into()));
        assert_eq!(bst(&Value::Package(integers(&[0, 0, 0]))), Err("a package of 3 elements, where §10.2.2 has at least 4".into()));
        assert_eq!(bst(&Value::Integer(0)), Err("no package".into()));
        assert_eq!(bst(&Value::Package(integers(&[0, 1 << 32, 0, 0]))), Err("its Battery Present Rate is wider than a DWORD".into()));
        let mut stringy = integers(&[0, 0, 0]);
        stringy.push(Value::String(b"12000".to_vec()));
        assert_eq!(bst(&Value::Package(stringy)), Err("its Battery Present Voltage is no Integer".into()));
        assert_eq!(bif(&bif_of([2, 0, 0, 0, 0, 0, 0, 0, 0])), Err("its Power Unit is 2, which §10.2.2 does not define".into()));
        assert_eq!(bix(&bix_of(0, [0; 8], 20)).map(|info| info.unit), Ok(Unit::Power));
        let Value::Package(mut short) = bix_of(0, [0; 8], 20) else { unreachable!() };
        short.pop();
        assert_eq!(bix(&Value::Package(short)), Err("a package of 19 elements, where §10.2.2 has at least 20".into()));
    }

    /// §6.1.5's compressed EISA ID, as `EISAID ("PNP0C0A")` compiles it, and
    /// a string `_HID` as it is.
    #[test]
    fn an_hid_is_read_as_its_text_whether_compressed_or_a_string() {
        assert_eq!(hid(&Value::Integer(0x0A0C_D041)).as_deref(), Some("PNP0C0A"));
        assert_eq!(hid(&Value::Integer(0x090C_D041)).as_deref(), Some("PNP0C09"));
        assert_eq!(hid(&Value::String(b"ACPI0003".to_vec())).as_deref(), Some("ACPI0003"));
        assert_eq!(hid(&Value::Integer(0x8000_0000_u32.swap_bytes().into())), None, "the reserved top bit set");
        assert_eq!(hid(&Value::Integer(1 << 32)), None);
        assert_eq!(hid(&Value::Buffer(b"PNP0C0A".to_vec())), None);
    }
}
