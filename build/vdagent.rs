//! QEMU's clipboard channel at the launch: its `qemu-vdagent` chardev, the
//! virtio-serial function of its own it is on, and the agent's port, from the
//! one declaration the guest's agent reads (`toyos-vdagent`).
//!
//! **A function of its own**, created just before the console's: the console's
//! function is the kernel's log channel from early boot, and the agent's
//! enumerates first, so every virtio boot runs the kernel's choice of console
//! (`toyos_pci::console`).
//!
//! **The chardev exists only in a QEMU built with spice-protocol** (QEMU's
//! `qapi/char.json`, `'if': 'CONFIG_SPICE_PROTOCOL'`), so a launch asks QEMU
//! for it first and refuses by name; nothing falls back.

use std::process::Command;
use std::sync::OnceLock;

use toyos_vdagent::{MAX_PORTS, PORT_ID, PORT_NAME};

use crate::arch::Arch;

/// The arguments that give a guest the channel. `platform` is the virtio
/// function's `iommu_platform` property, as every other virtio function on the
/// machine is given it.
pub fn args(platform: &str) -> [String; 6] {
    [
        "-chardev".into(),
        "qemu-vdagent,id=vdagent0,clipboard=on,mouse=off".into(),
        "-device".into(),
        format!("virtio-serial-pci-non-transitional,id=vdagent-serial,max_ports={MAX_PORTS}{platform}"),
        "-device".into(),
        format!("virtserialport,bus=vdagent-serial.0,nr={PORT_ID},chardev=vdagent0,name={PORT_NAME}"),
    ]
}

/// Whether `arch`'s QEMU has the chardev, asked once per process.
pub fn available(arch: Arch) -> Result<(), String> {
    static ASKED: [OnceLock<Result<(), String>>; 2] = [OnceLock::new(), OnceLock::new()];
    let slot = &ASKED[Arch::ALL.iter().position(|a| *a == arch).expect("every Arch is in ALL")];
    slot.get_or_init(|| {
        let listed = help(arch, "-chardev")?;
        named(&listed, "qemu-vdagent").then_some(()).ok_or_else(|| {
            format!(
                "{} has no `qemu-vdagent` chardev: it was built without spice-protocol, and every \
                 virtio machine carries the shared clipboard's channel",
                arch.qemu()
            )
        })
    })
    .clone()
}

/// Whether `arch`'s QEMU has the GTK display, which `cargo run` takes on a
/// host other than macOS for its clipboard.
pub fn gtk(arch: Arch) -> Result<(), String> {
    let listed = help(arch, "-display")?;
    named(&listed, "gtk").then_some(()).ok_or_else(|| {
        format!("{} has no GTK display; Debian ships it as `qemu-system-gui`", arch.qemu())
    })
}

/// What `<qemu> <option> help` lists.
fn help(arch: Arch, option: &str) -> Result<String, String> {
    let out = Command::new(arch.qemu())
        .args([option, "help"])
        .output()
        .map_err(|e| format!("{} {option} help: {e}", arch.qemu()))?;
    if !out.status.success() {
        return Err(format!(
            "{} {option} help: {}: {}",
            arch.qemu(),
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Whether a help listing names `backend` on a line of its own.
fn named(listed: &str, backend: &str) -> bool {
    listed.lines().any(|line| line.trim() == backend)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The launch's names and numbers are the codec's: the port QEMU names is
    /// the one `vd_agent.h` gives the agent, at the id and port count the
    /// agent drives.
    #[test]
    fn the_launch_spells_the_codecs_declaration() {
        assert_eq!(
            args(",iommu_platform=on"),
            [
                "-chardev",
                "qemu-vdagent,id=vdagent0,clipboard=on,mouse=off",
                "-device",
                "virtio-serial-pci-non-transitional,id=vdagent-serial,max_ports=2,iommu_platform=on",
                "-device",
                "virtserialport,bus=vdagent-serial.0,nr=1,chardev=vdagent0,name=com.redhat.spice.0",
            ]
        );
    }
}
