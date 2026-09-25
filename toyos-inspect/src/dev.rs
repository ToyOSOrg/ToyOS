//! The kernel's device inventory, rendered under `dev.*`.
//!
//! The one root no port answers for: the reader asks the kernel with a
//! `SysCap` carrying `Rights::INVENTORY` and renders its typed records here, so
//! the path layout is decided in the same pure crate as every owner's.
//!
//! ```text
//! dev.cpus, dev.memory.{total,used}_bytes
//! dev.pci.<addr>.{vendor,device,class,driver}
//! dev.usb.<controller addr>.port<n>.{function,speed,vendor,product}
//! dev.disk.<id>.blocks
//! dev.disk.<id>.part<index>.{type,unique,first_lba,lbas,state}
//! dev.class.<class>
//! ... and under a claimed device, holder.<pid> = <process name>
//! ```
//!
//! **Every address is one segment**, so a `*` never lines one address's parts
//! up against another kind's: a PCI function is `ssss:bb:dd:f` in hex — its
//! segment group, bus, device and function — and never contains a `.`.
//!
//! A PCI function's `driver` is `kernel` for one a kernel driver took, `none`
//! for one nobody drives, and `claimed` for one a process's claim holds; a
//! partition's `state` is `kernel`, `free` or `claimed` the same way. A claim
//! whose handle the kernel found in a table adds `holder.<pid>` under what it
//! holds; one moving between two tables adds nothing.
//!
//! **A path is rendered once or the inventory is refused** ([`Repeated`]): two
//! records that would render to one path are two things this layout cannot
//! tell apart, and the second one's fields are never shown as the first's.

use alloc::collections::BTreeMap;
use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::{String, ToString};

use toyos_abi::inventory::{
    Claimed, Driven, Holder, PartState, PciAddr, Record, UsbFunction, UsbSpeed,
};
use toyos_abi::part::{PartGuid, GUID_TEXT_LEN};
use toyos_abi::syscall::SYSINFO_HEADER_SIZE;

use crate::wire::Value;

/// The root every inventory path is under.
pub const ROOT: &str = "dev";

/// What `SYS_SYSINFO`'s ambient header says about the machine.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Machine {
    pub cpus: u32,
    pub memory_total: u64,
    pub memory_used: u64,
}

impl Machine {
    /// The header's total memory, used memory and CPU count.
    pub fn from_header(header: &[u8; SYSINFO_HEADER_SIZE]) -> Self {
        let decoded = toyos_abi::syscall::SysinfoHeader::decode(header);
        Self { memory_total: decoded.memory_total, memory_used: decoded.memory_used, cpus: decoded.cpus }
    }
}

/// Two records rendered to one path, which is named.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Repeated(pub String);

impl core::fmt::Display for Repeated {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "two inventory records render to {}, so neither is shown", self.0)
    }
}

fn addr(at: PciAddr) -> String {
    format!("{:04x}:{:02x}:{:02x}:{:x}", at.segment, at.bus, at.dev, at.func)
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

fn speed(s: UsbSpeed) -> &'static str {
    match s {
        UsbSpeed::Full => "full",
        UsbSpeed::Low => "low",
        UsbSpeed::High => "high",
        UsbSpeed::Super => "super",
        UsbSpeed::SuperPlusGen2x1 => "super-plus-gen2x1",
        UsbSpeed::SuperPlusGen1x2 => "super-plus-gen1x2",
        UsbSpeed::SuperPlusGen2x2 => "super-plus-gen2x2",
    }
}

/// Paths under `dev`, each written once.
struct Out(BTreeMap<String, Value>);

impl Out {
    fn put(&mut self, path: String, value: Value) -> Result<(), Repeated> {
        let path = format!("{ROOT}.{path}");
        if self.0.contains_key(&path) {
            return Err(Repeated(path));
        }
        self.0.insert(path, value);
        Ok(())
    }
}

/// The machine and every record as `dev.*` paths, sorted.
pub fn render(machine: &Machine, records: &[Record]) -> Result<BTreeMap<String, Value>, Repeated> {
    let mut out = Out(BTreeMap::new());
    out.put("cpus".into(), machine.cpus.into())?;
    out.put("memory.total_bytes".into(), machine.memory_total.into())?;
    out.put("memory.used_bytes".into(), machine.memory_used.into())?;

    // Where each partition renders, by the claim's own key for it.
    let mut parts: BTreeMap<(u32, [u8; 16]), String> = BTreeMap::new();
    for record in records {
        match record {
            Record::Pci(p) => {
                let at = format!("pci.{}", addr(p.at));
                out.put(format!("{at}.vendor"), format!("{:04x}", p.vendor).into())?;
                out.put(format!("{at}.device"), format!("{:04x}", p.device).into())?;
                out.put(
                    format!("{at}.class"),
                    format!("{:02x}:{:02x}:{:02x}", p.class, p.subclass, p.prog_if).into(),
                )?;
                let driver = match p.driven {
                    Driven::Free => "none",
                    Driven::Kernel => "kernel",
                    Driven::Claimed => "claimed",
                };
                out.put(format!("{at}.driver"), driver.into())?;
            }
            Record::Usb(u) => {
                let at = format!("usb.{}.port{}", addr(u.controller), u.port);
                let function = match u.function {
                    UsbFunction::Keyboard => "keyboard",
                    UsbFunction::Pointer => "pointer",
                    UsbFunction::Storage => "storage",
                };
                out.put(format!("{at}.function"), function.into())?;
                out.put(format!("{at}.speed"), speed(u.speed).into())?;
                out.put(format!("{at}.vendor"), format!("{:04x}", u.vendor).into())?;
                out.put(format!("{at}.product"), format!("{:04x}", u.product).into())?;
            }
            Record::Block(b) => {
                out.put(format!("disk.{}.blocks", b.device), b.blocks.into())?;
            }
            Record::Partition(p) => {
                let at = format!("disk.{}.part{}", p.device, p.index);
                out.put(format!("{at}.type"), guid(p.type_guid).into())?;
                out.put(format!("{at}.unique"), guid(p.unique_guid).into())?;
                out.put(format!("{at}.first_lba"), p.first_lba.into())?;
                out.put(format!("{at}.lbas"), p.lbas.into())?;
                let state = match p.state {
                    PartState::Free => "free",
                    PartState::Kernel => "kernel",
                    PartState::Claimed => "claimed",
                };
                out.put(format!("{at}.state"), state.into())?;
                parts.entry((p.device, p.unique_guid)).or_insert(at);
            }
            Record::Claim(_) => {}
        }
    }

    // One process holding two handles to one claim holds it once.
    let mut held: BTreeSet<(String, u32)> = BTreeSet::new();
    for record in records {
        let Record::Claim(c) = record else { continue };
        let at = match c.on {
            Claimed::Pci(at) => format!("pci.{}", addr(at)),
            Claimed::Class(class) => format!("class.{}", class.class_name()),
            // A claim on a partition no listed table carries is on nothing
            // this layout has a path for.
            Claimed::Partition { device, unique_guid } => match parts.get(&(device, unique_guid)) {
                Some(at) => at.clone(),
                None => continue,
            },
        };
        if held.insert((at.clone(), c.holder.pid)) {
            out.put(format!("{at}.holder.{}", c.holder.pid), name(&c.holder).into())?;
        }
    }
    Ok(out.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::check_path;
    use crate::Selector;
    use alloc::vec::Vec;
    use toyos_abi::inventory::{Block, Claim, Partition, Pci, Usb, NAME_BYTES};
    use toyos_abi::syscall::DeviceType;

    fn holder(pid: u32, s: &str) -> Holder {
        let mut name = [0; NAME_BYTES];
        name[..s.len()].copy_from_slice(s.as_bytes());
        Holder { pid, name }
    }

    const MACHINE: Machine = Machine { cpus: 2, memory_total: 1024, memory_used: 512 };
    const NIC: PciAddr = PciAddr { segment: 0, bus: 0, dev: 0x1f, func: 6 };
    const XHCI: PciAddr = PciAddr { segment: 0, bus: 0, dev: 4, func: 0 };

    fn pci(at: PciAddr, driven: Driven) -> Record {
        Record::Pci(Pci { at, vendor: 0x8086, device: 0x15fc, class: 2, subclass: 0, prog_if: 0, driven })
    }

    fn part(device: u32, index: u32, unique: u8, state: PartState) -> Record {
        Record::Partition(Partition {
            device,
            index,
            type_guid: [0x28; 16],
            unique_guid: [unique; 16],
            first_lba: 2048,
            lbas: 100,
            state,
        })
    }

    fn records() -> Vec<Record> {
        let netd = holder(9, "netd");
        alloc::vec![
            pci(NIC, Driven::Claimed),
            pci(XHCI, Driven::Kernel),
            pci(PciAddr { segment: 1, bus: 0x80, dev: 5, func: 0 }, Driven::Claimed),
            Record::Usb(Usb {
                controller: XHCI,
                port: 1,
                speed: UsbSpeed::High,
                vendor: 0x46f4,
                product: 1,
                function: UsbFunction::Storage,
            }),
            Record::Block(Block { device: 0, blocks: 4096 }),
            Record::Block(Block { device: 1, blocks: 4096 }),
            part(0, 0, 0xab, PartState::Kernel),
            part(1, 0, 0xcd, PartState::Claimed),
            part(1, 1, 0xef, PartState::Free),
            // The image `dd`'d to a second disk: one GUID, two partitions.
            part(0, 1, 0xcd, PartState::Free),
            Record::Claim(Claim { on: Claimed::Pci(NIC), holder: netd }),
            // The same claim through a second handle in the same table.
            Record::Claim(Claim { on: Claimed::Pci(NIC), holder: netd }),
            Record::Claim(Claim { on: Claimed::Class(DeviceType::VirtioSound), holder: holder(7, "soundd") }),
            Record::Claim(Claim {
                on: Claimed::Partition { device: 1, unique_guid: [0xcd; 16] },
                holder: holder(12, "test-runner"),
            }),
        ]
    }

    fn text(got: &BTreeMap<String, Value>, path: &str) -> Option<String> {
        got.get(path).map(|v| alloc::format!("{v}"))
    }

    #[test]
    fn every_record_renders_to_paths_the_grammar_accepts() {
        let got = render(&MACHINE, &records()).expect("no path twice");
        for path in got.keys() {
            check_path(path).unwrap_or_else(|why| panic!("{path}: {why}"));
        }
        let text = |p: &str| text(&got, p);
        assert_eq!(text("dev.cpus").as_deref(), Some("2"));
        assert_eq!(text("dev.memory.used_bytes").as_deref(), Some("512"));
        assert_eq!(text("dev.pci.0000:00:1f:6.driver").as_deref(), Some("claimed"));
        assert_eq!(text("dev.pci.0000:00:1f:6.holder.9").as_deref(), Some("netd"));
        assert_eq!(text("dev.pci.0000:00:1f:6.class").as_deref(), Some("02:00:00"));
        assert_eq!(text("dev.pci.0000:00:04:0.driver").as_deref(), Some("kernel"));
        // Claimed, and its handle was in no table the kernel walked.
        assert_eq!(text("dev.pci.0001:80:05:0.driver").as_deref(), Some("claimed"));
        assert!(!got.keys().any(|p| p.starts_with("dev.pci.0001:80:05:0.holder")));
        assert_eq!(text("dev.usb.0000:00:04:0.port1.function").as_deref(), Some("storage"));
        assert_eq!(text("dev.usb.0000:00:04:0.port1.speed").as_deref(), Some("high"));
        assert_eq!(text("dev.disk.1.blocks").as_deref(), Some("4096"));
        assert_eq!(text("dev.disk.0.part0.state").as_deref(), Some("kernel"));
        assert_eq!(text("dev.disk.1.part1.state").as_deref(), Some("free"));
        // One GUID on two disks is two partitions, and the claim is on the
        // one its device names.
        assert_eq!(text("dev.disk.1.part0.state").as_deref(), Some("claimed"));
        assert_eq!(text("dev.disk.1.part0.holder.12").as_deref(), Some("test-runner"));
        assert_eq!(text("dev.disk.0.part1.state").as_deref(), Some("free"));
        assert!(!got.keys().any(|p| p.starts_with("dev.disk.0.part1.holder")));
        assert_eq!(
            text("dev.disk.0.part1.unique").as_deref(),
            Some("cdcdcdcd-cdcd-cdcd-cdcd-cdcdcdcdcdcd")
        );
        assert_eq!(text("dev.class.virtio-sound.holder.7").as_deref(), Some("soundd"));
    }

    #[test]
    fn a_path_two_records_render_to_is_refused_by_name() {
        let mut twice = records();
        twice.push(part(1, 1, 0x99, PartState::Kernel));
        assert_eq!(
            render(&MACHINE, &twice),
            Err(Repeated("dev.disk.1.part1.type".into()))
        );
        let mut twice = records();
        twice.push(pci(NIC, Driven::Free));
        assert_eq!(
            render(&MACHINE, &twice),
            Err(Repeated("dev.pci.0000:00:1f:6.vendor".into()))
        );
    }

    /// An address is one segment: `*.0.*` is the one disk numbered 0, and no
    /// function 0, bus 0 or segment 0 of any other kind.
    #[test]
    fn a_star_never_lines_up_the_parts_of_an_address() {
        let got = render(&MACHINE, &records()).expect("no path twice");
        let s = Selector::parse("*.0.*").unwrap();
        let hits: Vec<&str> = got.keys().map(String::as_str).filter(|p| s.matches(p)).collect();
        assert!(!hits.is_empty());
        for path in &hits {
            assert!(path.starts_with("dev.disk.0."), "{path}");
        }
        let s = Selector::parse("dev.pci.*.driver").unwrap();
        assert_eq!(got.keys().filter(|p| s.matches(p)).count(), 3);
    }

    #[test]
    fn the_header_is_read_at_the_kernels_offsets() {
        let mut header = [0u8; SYSINFO_HEADER_SIZE];
        header[0..8].copy_from_slice(&(8u64 << 30).to_le_bytes());
        header[8..16].copy_from_slice(&(1u64 << 30).to_le_bytes());
        header[16..20].copy_from_slice(&4u32.to_le_bytes());
        header[20..24].copy_from_slice(&99u32.to_le_bytes());
        assert_eq!(
            Machine::from_header(&header),
            Machine { cpus: 4, memory_total: 8 << 30, memory_used: 1 << 30 }
        );
    }
}
