//! What machine this is, as each system can say it: its firmware's model, its
//! PCI functions, its USB devices, its NVMe controllers and the CPU idle
//! states the kernel uses. One `note=identity.<what>` line each. **No serial
//! number, UUID or network address is read**: a model is the same on every
//! machine of it, and a serial is this one's.

pub fn suite() {
    imp::suite();
}

#[cfg(target_os = "linux")]
mod imp {
    use std::fs;
    use std::path::Path;

    use crate::report::{condition, note};

    fn read(path: &Path) -> Option<String> {
        fs::read_to_string(path).ok().map(|s| s.trim().to_string())
    }

    fn get(dir: &Path, file: &str) -> String {
        read(&dir.join(file)).unwrap_or_else(|| "absent".into())
    }

    /// The last part of the link `dir/driver` names: the driver bound to it.
    fn driver(dir: &Path) -> String {
        fs::read_link(dir.join("driver"))
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "none".into())
    }

    /// Every entry of `dir`, sorted by name.
    fn entries(dir: &str) -> Vec<std::path::PathBuf> {
        let mut out: Vec<_> = fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
        out.sort();
        out
    }

    fn name(path: &Path) -> String {
        path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    fn hex(dir: &Path, file: &str) -> String {
        get(dir, file).trim_start_matches("0x").to_string()
    }

    pub fn suite() {
        let dmi = Path::new("/sys/class/dmi/id");
        for file in ["sys_vendor", "product_name", "product_version", "board_vendor", "board_name", "bios_vendor", "bios_version", "bios_date"] {
            condition(&format!("dmi.{file}"), get(dmi, file));
        }
        let cpuinfo = fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
        let microcode = cpuinfo.lines().find_map(|l| l.strip_prefix("microcode")?.split_once(':').map(|(_, v)| v.trim().to_string()));
        condition("cpu_microcode", microcode.unwrap_or_else(|| "absent".into()));

        for dev in entries("/sys/bus/pci/devices") {
            let id = format!("{}:{}", hex(&dev, "vendor"), hex(&dev, "device"));
            let subsystem = format!("{}:{}", hex(&dev, "subsystem_vendor"), hex(&dev, "subsystem_device"));
            note(
                "identity.pci",
                &[("at", name(&dev)), ("id", id), ("subsystem", subsystem), ("class", hex(&dev, "class")), ("driver", driver(&dev))],
            );
        }
        for dev in entries("/sys/bus/usb/devices").into_iter().filter(|d| d.join("idVendor").exists()) {
            let id = format!("{}:{}", get(&dev, "idVendor"), get(&dev, "idProduct"));
            note(
                "identity.usb",
                &[
                    ("at", name(&dev)),
                    ("id", id),
                    ("class", get(&dev, "bDeviceClass")),
                    ("speed_mbps", get(&dev, "speed")),
                    ("manufacturer", get(&dev, "manufacturer")),
                    ("product", get(&dev, "product")),
                ],
            );
        }
        for ctrl in entries("/sys/class/nvme") {
            note(
                "identity.nvme",
                &[("at", name(&ctrl)), ("model", get(&ctrl, "model")), ("firmware", get(&ctrl, "firmware_rev")), ("transport", get(&ctrl, "transport"))],
            );
        }
        for disk in entries("/sys/block").into_iter().filter(|d| name(d).starts_with("nvme")) {
            let bytes = get(&disk, "size").parse::<u64>().map_or("unknown".into(), |sectors| (sectors * 512).to_string());
            note("identity.nvme_namespace", &[("at", name(&disk)), ("bytes", bytes), ("logical_block", get(&disk, "queue/logical_block_size"))]);
        }
        let idle = Path::new("/sys/devices/system/cpu/cpuidle");
        let governor = read(&idle.join("current_governor")).or_else(|| read(&idle.join("current_governor_ro")));
        note("identity.cpuidle", &[("driver", get(idle, "current_driver")), ("governor", governor.unwrap_or_else(|| "absent".into()))]);
        for state in entries("/sys/devices/system/cpu/cpu0/cpuidle") {
            note(
                "identity.cpuidle_state",
                &[
                    ("cpu", "0".into()),
                    ("state", name(&state)),
                    ("name", get(&state, "name")),
                    ("desc", get(&state, "desc")),
                    ("latency_us", get(&state, "latency")),
                    ("residency_us", get(&state, "residency")),
                    ("disabled", get(&state, "disable")),
                ],
            );
        }
    }
}

#[cfg(target_os = "toyos")]
mod imp {
    use toyos_abi::inventory::{Driven, RawRecord, Record};

    use crate::report::{note, refuse, unsupported};

    pub fn suite() {
        pci();
        unsupported("identity.usb", "usbd keeps its devices and no right reads them from another program");
        unsupported("identity.nvme", "the NVMe driver keeps its controller's identity and no right reads it from another program");
        unsupported("identity.cpuidle", "the kernel names its idle states to no program");
    }

    /// Every PCI function off the kernel's inventory, which the runner's
    /// capability carries `inventory` for.
    fn pci() {
        let Some(cap) = crate::sys::syscap() else {
            return refuse("identity.pci", "this program was endowed no SysCap");
        };
        let records: Vec<Record> = match cap.records(|n| vec![RawRecord::EMPTY; n]) {
            Ok(records) => records,
            Err(why) => return refuse("identity.pci", format!("the inventory would not read: {why}")),
        };
        for record in records {
            let Record::Pci(f) = record else { continue };
            let driven = match f.driven {
                Driven::Free => "none",
                Driven::Kernel => "kernel",
                Driven::Claimed => "claimed",
            };
            note(
                "identity.pci",
                &[
                    ("at", format!("{:04x}:{:02x}:{:02x}.{:x}", f.at.segment, f.at.bus, f.at.dev, f.at.func)),
                    ("id", format!("{:04x}:{:04x}", f.vendor, f.device)),
                    ("class", format!("{:02x}{:02x}{:02x}", f.class, f.subclass, f.prog_if)),
                    ("driver", driven.into()),
                ],
            );
        }
    }
}
