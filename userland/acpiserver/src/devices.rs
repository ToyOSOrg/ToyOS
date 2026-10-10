//! The devices this server serves that only the machine's namespace names:
//! the embedded controller (ACPI 6.5 §12.11), whose ports and GPE no table the
//! kernel reads gives, and a power button that is a control method device
//! (§4.8.2.2.1.2), which the AML that notifies it is the only account of.
//!
//! **A device is one whose `_HID` names it** (§6.1.5): an EISA ID's integer
//! or a string, PNP0C09 for the controller and PNP0C0C for the button, and
//! **present where its `_STA` says so** (§6.3.7), bit 0, a device with none
//! being present.
//!
//! **The controller is its `_CRS`'s two fixed ports and its `_GPE`'s
//! integer**: §12.11 puts the data register first and the command/status
//! register second, and a `_GPE` that is a package names a GPE block device,
//! which this server does not serve. The GPE is held to the FADT's GPE0
//! block, eight a byte of its status half. A machine with more than one
//! controller present has none served. Whatever keeps a controller from
//! being served is said once, by kind; what the firmware chose — a path, a
//! number — goes under [`OWN`].
//!
//! **Buttons are looked for only where the FADT says the power button is a
//! control method device**; on a machine whose button is the fixed one, a
//! PNP0C0C the firmware lists anyway is not another way to press it.

use std::collections::BTreeSet;

use toyos_abi::acpi::Block;
use acpiserver_api::CONTROLLER_NONE;
use toyos_aml::{Host, Interpreter, Kind, Value};

use crate::aml::kind;
use crate::host::OWN;

/// The embedded controller, as its device names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ec {
    pub path: String,
    pub command: u16,
    pub data: u16,
    pub gpe: u16,
    /// The queries its device defines a method for.
    pub queries: BTreeSet<u8>,
}

/// What was found.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Found {
    pub ec: Option<Ec>,
    /// Every control-method power button present, by path.
    pub buttons: Vec<String>,
}

const EC: &str = "PNP0C09";
const POWER_BUTTON: &str = "PNP0C0C";

/// Find the controller, and the buttons where `control_method`, in the
/// namespace `interpreter` holds; `gpe0` is the block every GPE is in.
pub fn find(interpreter: &mut Interpreter, host: &mut dyn Host, gpe0: Block, control_method: bool) -> Found {
    let mut found = Found::default();
    let (devices, named) = match devices(interpreter) {
        Ok(walked) => walked,
        Err(why) => {
            println!("acpiserver: no device of the namespace is served: its walk was refused, {why}");
            return found;
        }
    };
    let mut controllers = Vec::new();
    for device in devices {
        let hid = format!("{device}._HID");
        if !named.contains(&hid) {
            continue;
        }
        let id = match interpreter.evaluate(host, &hid, &[]) {
            Ok(value) => id(&value),
            Err(why) => {
                println!("{OWN}{hid} was refused {why:x?}");
                println!("acpiserver: a device's _HID did not evaluate, so it is no device this server serves: {}", kind(&why));
                continue;
            }
        };
        let wanted = match id.as_deref() {
            Some(EC) => true,
            Some(POWER_BUTTON) => control_method,
            _ => false,
        };
        if !wanted || !present(interpreter, host, &named, &device) {
            continue;
        }
        match id.as_deref() {
            Some(EC) => controllers.push(device),
            _ => {
                println!("{OWN}{device} is the control-method power button");
                found.buttons.push(device);
            }
        }
    }
    found.ec = match controllers.as_slice() {
        [] => {
            println!("{CONTROLLER_NONE}no PNP0C09 device is present");
            None
        }
        [device] => match controller(interpreter, host, device, gpe0, &named) {
            Ok(ec) => {
                println!("{OWN}{device} is the embedded controller");
                Some(ec)
            }
            Err(why) => {
                println!("{CONTROLLER_NONE}its device {why}");
                None
            }
        },
        more => {
            println!("{CONTROLLER_NONE}{} PNP0C09 devices are present, and this server serves one", more.len());
            None
        }
    };
    found
}

/// What became of an embedded-controller query's method.
#[derive(Debug, PartialEq, Eq)]
pub enum Queried {
    Ran,
    /// The controller's device defines none for the query.
    Absent,
    /// By the kind of its refusal.
    Refused(String),
}

/// Run the method the controller's device defines for query `q` (§12.11,
/// `_Qxx`, the number in two hex digits): its Notifies are the host's.
pub fn query(interpreter: &mut Interpreter, host: &mut dyn Host, ec: &Ec, q: u8) -> Queried {
    if !ec.queries.contains(&q) {
        return Queried::Absent;
    }
    match interpreter.evaluate(host, &format!("{}._Q{q:02X}", ec.path), &[]) {
        Ok(_) => Queried::Ran,
        Err(why) => {
            println!("{OWN}{}._Q{q:02X} was refused {why:x?}", ec.path);
            Queried::Refused(kind(&why))
        }
    }
}

/// Every device's path, and every name's, by one walk.
fn devices(interpreter: &Interpreter) -> Result<(Vec<String>, BTreeSet<String>), String> {
    let mut walk = interpreter.walk().map_err(|why| kind(&why))?;
    let (mut devices, mut named) = (Vec::new(), BTreeSet::new());
    while let Some(entry) = walk.next() {
        if entry.kind == Kind::Device {
            devices.push(walk.path().to_string());
        }
        named.insert(walk.path().to_string());
    }
    Ok((devices, named))
}

/// A `_HID` as the text it names (§6.1.5): a string as it is, and an integer
/// as the EISA ID it compresses, three letters of five bits each and four
/// hex digits, its bytes in the order AML stores them.
fn id(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => String::from_utf8(text.clone()).ok(),
        &Value::Integer(compressed) => {
            let [vendor_hi, vendor_lo, product_hi, product_lo] = u32::try_from(compressed).ok()?.to_le_bytes();
            let vendor = u16::from_be_bytes([vendor_hi, vendor_lo]);
            let letter = |shift: u16| char::from(b'@' + (vendor >> shift & 0x1F) as u8);
            Some(format!("{}{}{}{product_hi:02X}{product_lo:02X}", letter(10), letter(5), letter(0)))
        }
        _ => None,
    }
}

/// Whether `device` is present: bit 0 of its `_STA`, which it is where it has
/// none; one whose `_STA` does not evaluate to an integer is said and is not.
fn present(interpreter: &mut Interpreter, host: &mut dyn Host, named: &BTreeSet<String>, device: &str) -> bool {
    let sta = format!("{device}._STA");
    if !named.contains(&sta) {
        return true;
    }
    match interpreter.evaluate(host, &sta, &[]) {
        Ok(Value::Integer(status)) => status & 1 != 0,
        Ok(_) => {
            println!("{OWN}{sta} is no integer");
            println!("acpiserver: a device's _STA is no integer, so it is taken as absent");
            false
        }
        Err(why) => {
            println!("{OWN}{sta} was refused {why:x?}");
            println!("acpiserver: a device's _STA did not evaluate, so it is taken as absent: {}", kind(&why));
            false
        }
    }
}

/// The controller `device` names, or why it names none this server serves.
fn controller(interpreter: &mut Interpreter, host: &mut dyn Host, device: &str, gpe0: Block, named: &BTreeSet<String>) -> Result<Ec, String> {
    let crs = match interpreter.evaluate(host, &format!("{device}._CRS"), &[]) {
        Ok(Value::Buffer(list)) => list,
        Ok(_) => return Err("names its resources by no buffer".into()),
        Err(why) => return Err(format!("names no resources: {}", kind(&why))),
    };
    let mut ports = [0u16; 2];
    match toyos_acpi::io_ports(&crs[..], 0, &mut ports) {
        Ok(2) => {}
        Ok(n) => return Err(format!("names {n} I/O run(s), not the two of §12.11")),
        Err(why) => return Err(format!("names its ports by a list this server does not read: {why}")),
    }
    let [data, command] = ports;
    // §5.2.9: a GPE block's status half holds eight GPEs a byte.
    let gpes = u64::from(gpe0.len / 2 * 8);
    let gpe = match interpreter.evaluate(host, &format!("{device}._GPE"), &[]) {
        Ok(Value::Integer(gpe)) if gpe < gpes => gpe as u16,
        Ok(Value::Integer(gpe)) => {
            println!("{OWN}{device}._GPE is {gpe:#x}");
            return Err(format!("raises a GPE outside GPE0's {gpes}"));
        }
        Ok(Value::Package(_)) => return Err("raises its GPE in a GPE block device, which this server does not serve".into()),
        Ok(_) => return Err("names its GPE by no integer".into()),
        Err(why) => return Err(format!("names no GPE: {}", kind(&why))),
    };
    let queries = (0..=u8::MAX).filter(|q| named.contains(&format!("{device}._Q{q:02X}"))).collect();
    Ok(Ec { path: device.into(), command, data, gpe, queries })
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::aml::tests::{cat, crafted, sealed, CRAFTED_RSDP};
    use crate::aml::{load, Aml};

    /// GPE0 as an AMD laptop's FADT names it: eight bytes, 32 GPEs.
    pub const GPE0: Block = Block { port: 0x420, len: 8 };

    /// A NameString of segments from the root: `\` and a MultiNamePrefix.
    pub fn path(segments: &[&[u8; 4]]) -> Vec<u8> {
        let mut out = vec![b'\\'];
        match segments.len() {
            1 => {}
            2 => out.push(0x2E),
            n => out.extend_from_slice(&[0x2F, n as u8]),
        }
        segments.iter().for_each(|segment| out.extend_from_slice(*segment));
        out
    }

    /// A PkgLength of one to four bytes, for `len` bytes after it (§20.2.4).
    pub fn pkg(body: &[u8]) -> Vec<u8> {
        let mut out = match body.len() + 1 {
            n if n < 0x40 => vec![n as u8],
            _ => {
                let n = body.len() + 2;
                assert!(n < 0x1000, "a test package past two length bytes");
                vec![0x40 | (n & 0xF) as u8, (n >> 4) as u8]
            }
        };
        out.extend_from_slice(body);
        out
    }

    /// `Device (<path>) { <body> }`.
    pub fn device(at: &[&[u8; 4]], body: &[u8]) -> Vec<u8> {
        cat(&[&[0x5B, 0x82], &pkg(&cat(&[&path(at), body]))])
    }

    /// `Name (<name>, <dword>)`, the form `EisaId` and `_STA` take.
    pub fn dword(name: &[u8; 4], value: u32) -> Vec<u8> {
        cat(&[&[0x08], name, &[0x0C], &value.to_le_bytes()])
    }

    /// `Name (<name>, <byte>)`.
    pub fn byte(name: &[u8; 4], value: u8) -> Vec<u8> {
        cat(&[&[0x08], name, &[0x0A, value]])
    }

    /// `Name (_CRS, ResourceTemplate () { IO (Decode16, p, p, 0, 1) ... })`.
    pub fn crs(ports: &[u16]) -> Vec<u8> {
        let mut list: Vec<u8> = ports.iter().flat_map(|p| { let [lo, hi] = p.to_le_bytes(); [0x47, 0x01, lo, hi, lo, hi, 0x00, 0x01] }).collect();
        list.extend_from_slice(&[0x79, 0x00]);
        let buffer = cat(&[&[0x0A, list.len() as u8], &list]);
        cat(&[&[0x08], b"_CRS", &[0x11], &pkg(&buffer)])
    }

    /// `EisaId ("PNP0C09")` and `EisaId ("PNP0C0C")`, as an AMD laptop's DSDT
    /// stores them.
    pub const PNP0C09: u32 = 0x090C_D041;
    pub const PNP0C0C: u32 = 0x0C0C_D041;

    /// An AMD laptop's controller and power button, decoded: the controller
    /// at `\_SB.PCI0.LPC0.EC` with `_HID` PNP0C09, an `_STA` of 0x0F, its
    /// `_CRS` 0x62 then 0x66 and `_GPE` 3; and the button at `\_SB.PWRB`
    /// with `_HID` PNP0C0C and an `_STA` of 0x0B.
    pub fn laptop(extra_ec: &[u8]) -> Vec<u8> {
        cat(&[
            &device(&[b"_SB_", b"PCI0"], &[]),
            &device(&[b"_SB_", b"PCI0", b"LPC0"], &[]),
            &device(&[b"_SB_", b"PCI0", b"LPC0", b"EC__"], &cat(&[&dword(b"_HID", PNP0C09), &byte(b"_STA", 0x0F), &crs(&[0x62, 0x66]), &byte(b"_GPE", 3), extra_ec])),
            &device(&[b"_SB_", b"PWRB"], &cat(&[&dword(b"_HID", PNP0C0C), &byte(b"_STA", 0x0B)])),
        ])
    }

    pub const EC_PATH: &str = "\\_SB_.PCI0.LPC0.EC__";
    pub const PWRB: &str = "\\_SB_.PWRB";

    fn found(dsdt: &[u8], control_method: bool) -> Found {
        let kernel = crafted(&sealed(b"DSDT", dsdt), &[]);
        let (_, kept) = load(&kernel, CRAFTED_RSDP);
        let Aml { mut interpreter, mut host } = kept.expect("the DSDT loads");
        find(&mut interpreter, &mut host, GPE0, control_method)
    }

    #[test]
    fn an_amd_laptops_controller_and_button_are_found_in_its_dsdt() {
        let ec = Ec { path: EC_PATH.into(), command: 0x66, data: 0x62, gpe: 3, queries: BTreeSet::new() };
        assert_eq!(found(&laptop(&[]), true), Found { ec: Some(ec.clone()), buttons: vec![PWRB.into()] });
        assert_eq!(found(&laptop(&[]), false), Found { ec: Some(ec), buttons: vec![] }, "a fixed button's machine looks for none");
    }

    /// An AMD laptop's `_Q28`, decoded: its query's number stored to the POST
    /// port as a word, then `Notify (\_SB.PWRB, 0x80)`; beside it `_Q2A`,
    /// whose Notify is the lid's.
    pub fn queries() -> Vec<u8> {
        let region = cat(&[&[0x5B, 0x80], b"P80R", &[0x01, 0x0A, 0x80, 0x0A, 0x02]]);
        let field = cat(&[&[0x5B, 0x81], &pkg(&cat(&[b"P80R", &[0x02], b"P80H", &[0x10]]))]);
        let method = |name: &[u8; 4], q: u8, notified: &[u8]| {
            let store = cat(&[&[0x70, 0x0A, q], b"P80H"]);
            let notify = cat(&[&[0x86], notified, &[0x0A, 0x80]]);
            cat(&[&[0x14], &pkg(&cat(&[name, &[0x00], &store, &notify]))])
        };
        cat(&[&region, &field, &method(b"_Q28", 0x28, &path(&[b"_SB_", b"PWRB"])), &method(b"_Q2A", 0x2A, &path(&[b"_SB_", b"LID_"]))])
    }

    /// The laptop's DSDT with [`queries`] in its controller, and the lid.
    pub fn laptop_with_queries() -> Vec<u8> {
        cat(&[&device(&[b"_SB_", b"LID_"], &dword(b"_HID", 0x0D0C_D041)), &laptop(&queries())])
    }

    /// Query 0x28's method runs, writes its word to the POST port through the
    /// kernel and notifies the button, which the host counts as a press; 0x2A's
    /// notifies the lid, which is none; and a query the controller defines no
    /// method for runs nothing.
    #[test]
    fn query_0x28_on_an_amd_laptop_is_a_press_of_its_power_button() {
        let mut kernel = crafted(&sealed(b"DSDT", &laptop_with_queries()), &[]);
        kernel.ports = vec![(0x80, 0)];
        let (_, kept) = load(&kernel, CRAFTED_RSDP);
        let Aml { mut interpreter, mut host } = kept.expect("the DSDT loads");
        let found = find(&mut interpreter, &mut host, GPE0, true);
        let ec = found.ec.expect("the controller");
        assert_eq!(ec.queries, BTreeSet::from([0x28, 0x2A]));
        host.buttons = found.buttons;
        assert_eq!(query(&mut interpreter, &mut host, &ec, 0x2A), Queried::Ran);
        assert_eq!(host.presses, 0, "the lid's Notify is no press");
        assert_eq!(query(&mut interpreter, &mut host, &ec, 0x28), Queried::Ran);
        assert_eq!(host.presses, 1);
        let wrote = toyos_abi::acpi::Access::write(toyos_abi::acpi::Space::SystemIo, 0x80, toyos_abi::acpi::Width::Word, 0x28);
        assert_eq!(kernel.asked.borrow().iter().filter(|access| access.write == 1).last(), Some(&wrote));
        assert_eq!(query(&mut interpreter, &mut host, &ec, 0x29), Queried::Absent);
        assert_eq!(host.presses, 1);
    }

    /// A port the kernel refuses ends the method before its Notify: no press,
    /// and the refusal by its kind.
    #[test]
    fn a_query_whose_write_the_kernel_refuses_is_no_press() {
        let kernel = crafted(&sealed(b"DSDT", &laptop_with_queries()), &[]);
        let (_, kept) = load(&kernel, CRAFTED_RSDP);
        let Aml { mut interpreter, mut host } = kept.expect("the DSDT loads");
        let found = find(&mut interpreter, &mut host, GPE0, true);
        host.buttons = found.buttons;
        let refused = query(&mut interpreter, &mut host, found.ec.as_ref().expect("the controller"), 0x28);
        assert_eq!(refused, Queried::Refused("denied by this server: a SystemIO write the kernel refused KernelPort".into()));
        assert_eq!(host.presses, 0);
    }

    #[test]
    fn an_eisa_id_is_its_three_letters_and_four_digits() {
        assert_eq!(id(&Value::Integer(u64::from(PNP0C09))).as_deref(), Some("PNP0C09"));
        assert_eq!(id(&Value::Integer(u64::from(PNP0C0C))).as_deref(), Some("PNP0C0C"));
        assert_eq!(id(&Value::String(b"PNP0C09".to_vec())).as_deref(), Some("PNP0C09"));
        assert_eq!(id(&Value::Integer(1 << 32)), None);
        assert_eq!(id(&Value::Buffer(b"PNP0C09".to_vec())), None);
    }

    /// Absent devices are not served, and a controller whose resources or
    /// GPE are none this server serves is none, by name.
    #[test]
    fn a_controller_this_server_cannot_serve_is_none() {
        let absent = cat(&[&device(&[b"_SB_", b"EC__"], &cat(&[&dword(b"_HID", PNP0C09), &byte(b"_STA", 0x0E), &crs(&[0x62, 0x66]), &byte(b"_GPE", 3)]))]);
        assert_eq!(found(&absent, true), Found::default());
        let ec = |crs_ports: &[u16], gpe: u8| cat(&[&device(&[b"_SB_", b"EC__"], &cat(&[&dword(b"_HID", PNP0C09), &crs(crs_ports), &byte(b"_GPE", gpe)]))]);
        assert_eq!(found(&ec(&[0x62, 0x66], 32), true).ec, None, "GPE 32 is past an eight-byte GPE0");
        assert_eq!(found(&ec(&[0x62, 0x66], 31), true).ec.map(|ec| ec.gpe), Some(31));
        assert_eq!(found(&ec(&[0x62], 3), true).ec, None, "one port");
        assert_eq!(found(&ec(&[0x62, 0x66, 0x68], 3), true).ec, None, "three ports");
        let two = cat(&[&ec(&[0x62, 0x66], 3), &device(&[b"_SB_", b"EC2_"], &cat(&[&dword(b"_HID", PNP0C09), &crs(&[0x68, 0x6C]), &byte(b"_GPE", 4)]))]);
        assert_eq!(found(&two, true).ec, None, "two controllers present");
        let no_button = cat(&[&device(&[b"_SB_", b"PWRB"], &cat(&[&dword(b"_HID", PNP0C0C), &byte(b"_STA", 0)]))]);
        assert_eq!(found(&no_button, true).buttons, Vec::<String>::new(), "a button whose _STA says absent");
    }
}
