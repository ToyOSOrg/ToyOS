//! The kernel's device inventory, rendered under `dev.*`.
//!
//! The one root no port answers for: the reader asks the kernel with a
//! `SysCap` carrying `Rights::INVENTORY` and renders its typed records here, so
//! the path layout is decided in the same pure crate as every owner's.
//!
//! ```text
//! dev.cpus, dev.memory.{total,used}_bytes
//! dev.pci.<bb:dd.f>.{vendor,device,class,driver[,pid]}
//! dev.usb.<controller bb:dd.f>.port<n>.{function,speed,vendor,product}
//! dev.block.<id>.blocks
//! dev.part.<unique guid>.{device,type,first_lba,lbas,state[,holder,pid]}
//! dev.class.<class>.{holder,pid}
//! ```
//!
//! A PCI function's `driver` is the process holding its claim, `kernel` for a
//! function a kernel driver took, `none` for one nobody drives, and `claimed`
//! for one whose claim was in no process's table when the kernel looked.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};

use toyos_abi::inventory::{Bdf, Claim, Claimed, Driven, Holder, PartState, Record, UsbFunction};
use toyos_abi::part::{PartGuid, GUID_TEXT_LEN};
use toyos_abi::syscall::DeviceType;

use crate::wire::Value;

/// The root every inventory path is under.
pub const ROOT: &str = "dev";

fn bdf(at: Bdf) -> String {
    format!("{:02x}:{:02x}.{}", at.bus, at.dev, at.func)
}

fn guid(bytes: [u8; 16]) -> String {
    let mut buf = [0u8; GUID_TEXT_LEN];
    PartGuid(bytes).write_text(&mut buf).to_ascii_lowercase()
}

/// A holder's name as a value: its bytes up to the first NUL, or `pid<n>` for
/// a name that is not text a line can carry.
fn name(holder: &Holder) -> String {
    match holder.name() {
        Some(n) if !n.is_empty() && !n.chars().any(char::is_control) => n.to_string(),
        _ => format!("pid{}", holder.pid),
    }
}

/// Every record as `dev.*` paths, sorted.
pub fn render(records: &[Record]) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    let mut put = |path: String, value: Value| {
        out.insert(format!("{ROOT}.{path}"), value);
    };
    let claim_of = |on: Claimed| -> Option<&Claim> {
        records.iter().find_map(|r| match r {
            Record::Claim(c) if c.on == on => Some(c),
            _ => None,
        })
    };
    for record in records {
        match record {
            Record::Machine(m) => {
                put("cpus".into(), m.cpus.into());
                put("memory.total_bytes".into(), m.memory_total.into());
                put("memory.used_bytes".into(), m.memory_used.into());
            }
            Record::Pci(p) => {
                let at = format!("pci.{}", bdf(p.at));
                put(format!("{at}.vendor"), format!("{:04x}", p.vendor).into());
                put(format!("{at}.device"), format!("{:04x}", p.device).into());
                put(
                    format!("{at}.class"),
                    format!("{:02x}:{:02x}:{:02x}", p.class, p.subclass, p.prog_if).into(),
                );
                let driver = match p.driven {
                    Driven::Free => "none".to_string(),
                    Driven::Kernel => "kernel".to_string(),
                    Driven::Claimed => match claim_of(Claimed::Pci(p.at)) {
                        Some(c) => {
                            put(format!("{at}.pid"), c.holder.pid.into());
                            name(&c.holder)
                        }
                        None => "claimed".to_string(),
                    },
                };
                put(format!("{at}.driver"), driver.into());
            }
            Record::Usb(u) => {
                let at = format!("usb.{}.port{}", bdf(u.controller), u.port);
                let function = match u.function {
                    UsbFunction::Keyboard => "keyboard",
                    UsbFunction::Pointer => "pointer",
                    UsbFunction::Storage => "storage",
                };
                put(format!("{at}.function"), function.into());
                let speed = match u.speed {
                    1 => "full".to_string(),
                    2 => "low".to_string(),
                    3 => "high".to_string(),
                    4 => "super".to_string(),
                    other => format!("psi{other}"),
                };
                put(format!("{at}.speed"), speed.into());
                put(format!("{at}.vendor"), format!("{:04x}", u.vendor).into());
                put(format!("{at}.product"), format!("{:04x}", u.product).into());
            }
            Record::Block(b) => {
                put(format!("block.{}.blocks", b.device), b.blocks.into());
            }
            Record::Partition(p) => {
                let at = format!("part.{}", guid(p.unique_guid));
                put(format!("{at}.device"), p.device.into());
                put(format!("{at}.type"), guid(p.type_guid).into());
                put(format!("{at}.first_lba"), p.first_lba.into());
                put(format!("{at}.lbas"), p.lbas.into());
                let state = match p.state {
                    PartState::Free => "free",
                    PartState::Mounted => "mounted",
                    PartState::Claimed => "claimed",
                };
                put(format!("{at}.state"), state.into());
                if let Some(c) = claim_of(Claimed::Partition(p.unique_guid)) {
                    put(format!("{at}.holder"), name(&c.holder).into());
                    put(format!("{at}.pid"), c.holder.pid.into());
                }
            }
            Record::Claim(c) => {
                if let Claimed::Class(raw) = c.on {
                    let class = match DeviceType::from_raw(u64::from(raw)) {
                        Some(class) => class.class_name().to_string(),
                        None => format!("class{raw}"),
                    };
                    put(format!("class.{class}.holder"), name(&c.holder).into());
                    put(format!("class.{class}.pid"), c.holder.pid.into());
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::check_path;
    use toyos_abi::inventory::{Block, Machine, Partition, Pci, Usb, NAME_BYTES};

    fn holder(pid: u32, s: &str) -> Holder {
        let mut name = [0; NAME_BYTES];
        name[..s.len()].copy_from_slice(s.as_bytes());
        Holder { pid, name }
    }

    fn records() -> [Record; 9] {
        let nic = Bdf { bus: 0, dev: 0x1f, func: 6 };
        let pci = |at, driven| {
            Record::Pci(Pci { at, vendor: 0x8086, device: 0x15fc, class: 2, subclass: 0, prog_if: 0, driven })
        };
        [
            Record::Machine(Machine { cpus: 2, memory_total: 1024, memory_used: 512 }),
            pci(nic, Driven::Claimed),
            pci(Bdf { bus: 0, dev: 4, func: 0 }, Driven::Kernel),
            pci(Bdf { bus: 0, dev: 5, func: 0 }, Driven::Claimed),
            Record::Usb(Usb {
                controller: Bdf { bus: 0, dev: 4, func: 0 },
                port: 1,
                speed: 3,
                vendor: 0x46f4,
                product: 1,
                function: UsbFunction::Storage,
            }),
            Record::Block(Block { device: 1, blocks: 4096 }),
            Record::Partition(Partition {
                device: 1,
                type_guid: [0x28; 16],
                unique_guid: [0xab; 16],
                first_lba: 2048,
                lbas: 100,
                state: PartState::Mounted,
            }),
            Record::Claim(Claim { on: Claimed::Pci(nic), holder: holder(9, "netd") }),
            Record::Claim(Claim { on: Claimed::Class(6), holder: holder(7, "soundd") }),
        ]
    }

    #[test]
    fn every_record_renders_to_paths_the_grammar_accepts() {
        let got = render(&records());
        for path in got.keys() {
            check_path(path).unwrap_or_else(|why| panic!("{path}: {why}"));
        }
        let text = |p: &str| got.get(p).map(|v| alloc::format!("{v}"));
        assert_eq!(text("dev.cpus").as_deref(), Some("2"));
        assert_eq!(text("dev.pci.00:1f.6.driver").as_deref(), Some("netd"));
        assert_eq!(text("dev.pci.00:1f.6.pid").as_deref(), Some("9"));
        assert_eq!(text("dev.pci.00:1f.6.class").as_deref(), Some("02:00:00"));
        assert_eq!(text("dev.pci.00:04.0.driver").as_deref(), Some("kernel"));
        // Claimed, and its handle was in no table the kernel walked.
        assert_eq!(text("dev.pci.00:05.0.driver").as_deref(), Some("claimed"));
        assert_eq!(text("dev.usb.00:04.0.port1.function").as_deref(), Some("storage"));
        assert_eq!(text("dev.usb.00:04.0.port1.speed").as_deref(), Some("high"));
        assert_eq!(text("dev.block.1.blocks").as_deref(), Some("4096"));
        let part = "dev.part.abababab-abab-abab-abab-abababababab";
        assert_eq!(text(&alloc::format!("{part}.state")).as_deref(), Some("mounted"));
        assert_eq!(text("dev.class.virtio-sound.holder").as_deref(), Some("soundd"));
    }
}
