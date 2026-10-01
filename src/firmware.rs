//! The UEFI firmware a guest boots: the Debian edk2 build [`pinned`] for its
//! machine and accelerator, committed under `qemu-firmware/` and recorded in
//! `NOTICE`. It is the half of the instrument `.github/qemu-version` does not
//! declare, so the dev host and CI boot the same bytes, and no guest boots what
//! the host's QEMU installation carries.
//!
//! Each pin, a build here or `.github/qemu-version`, moves only on a
//! measurement under every accelerator it serves. Firmware reads the CPU the
//! accelerator presents, and QEMU's HVF presents `virt`'s with
//! `ID_AA64PFR0_EL1.GIC` 0 where TCG presents 1.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::arch::{Accel, Arch};

/// One committed build: where it is under the repository's root, its code and
/// variable-store images, its `Firmware::flash`, and its images as a guest is
/// handed them, staged once per process.
struct Pin {
    dir: &'static str,
    code: &'static str,
    vars: &'static str,
    flash: Option<u64>,
    staged: OnceLock<Result<Firmware, String>>,
}

impl Pin {
    const fn new(dir: &'static str, code: &'static str, vars: &'static str, flash: Option<u64>) -> Pin {
        Pin { dir, code, vars, flash, staged: OnceLock::new() }
    }
}

static Q35: Pin = Pin::new("qemu-firmware/debian-2026.05-2", "OVMF_CODE_4M.fd", "OVMF_VARS_4M.fd", None);
static VIRT: Pin = Pin::new("qemu-firmware/debian-2026.05-2", "QEMU_EFI.fd", "QEMU_VARS.fd", Some(VIRT_FLASH));
static VIRT_HVF: Pin = Pin::new("qemu-firmware/debian-2024.11-5", "QEMU_EFI.fd", "QEMU_VARS.fd", Some(VIRT_FLASH));

/// The build a guest of `arch` boots under `accel`.
fn pinned(arch: Arch, accel: Accel) -> &'static Pin {
    match (arch, accel) {
        (Arch::X86_64, Accel::Kvm | Accel::Hvf | Accel::Tcg) => &Q35,
        (Arch::Aarch64, Accel::Kvm | Accel::Tcg) => &VIRT,
        // From edk2-stable202502 to stable202608 ArmVirtQemu never returns from
        // `ExitBootServices` under HVF
        // (`issues/build/edk2-stable202502-to-202608-hangs-at-exitbootservices-under-hvf.md`).
        (Arch::Aarch64, Accel::Hvf) => &VIRT_HVF,
    }
}

/// What each of `virt`'s two flash devices holds: QEMU's `VIRT_FLASH` window,
/// 128 MiB, halved. A file of any other size is refused, so Debian pads its
/// own copies of both images to this with zeros, and so does this module.
const VIRT_FLASH: u64 = 64 << 20;

/// One machine's firmware.
#[derive(Debug, PartialEq, Eq)]
pub struct Firmware {
    /// The code image, which a guest only ever reads.
    pub code: PathBuf,
    /// The variable-store template, which no guest is handed: each boot writes
    /// a copy of its own ([`Firmware::fresh_vars`]).
    vars: PathBuf,
    /// The size of the flash device each image fills, where the machine fixes
    /// one; q35 sizes its devices by the images.
    flash: Option<u64>,
}

impl Firmware {
    /// A fresh variable store at `to`, from the template.
    pub fn fresh_vars(&self, to: &Path) -> Result<(), String> {
        let template = fs::read(&self.vars)
            .map_err(|e| format!("read the firmware's variable store {}: {e}", self.vars.display()))?;
        write_padded(to, &template, self.flash)
    }

    /// The two `-drive` values that give a guest this firmware, with `vars` as
    /// its writable variable store.
    pub fn drives(&self, vars: &Path) -> [String; 2] {
        [
            format!("if=pflash,format=raw,unit=0,file={},readonly=on", self.code.display()),
            format!("if=pflash,format=raw,unit=1,file={},readonly=off", vars.display()),
        ]
    }
}

/// The firmware a guest of `arch` boots under `accel`.
pub fn of(arch: Arch, accel: Accel) -> Result<&'static Firmware, String> {
    let pin = pinned(arch, accel);
    pin.staged.get_or_init(|| stage(Path::new(env!("CARGO_MANIFEST_DIR")), pin)).as_ref().map_err(Clone::clone)
}

/// `pin`'s committed images, and for a machine whose flash is fixed, the code
/// image padded to it under `root`'s `target/`: written beside the last copy
/// and renamed over it, so a guest that opened that one reads it whole.
fn stage(root: &Path, pin: &Pin) -> Result<Firmware, String> {
    let Pin { dir, code, vars, flash, .. } = *pin;
    let committed = root.join(dir);
    let code = match flash {
        None => committed.join(code),
        Some(_) => {
            let bytes = fs::read(committed.join(code))
                .map_err(|e| format!("read the firmware {}: {e}", committed.join(code).display()))?;
            let dir = root.join("target").join(dir);
            fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
            let (part, to) = (dir.join(format!("{code}.{}", std::process::id())), dir.join(code));
            write_padded(&part, &bytes, flash)?;
            fs::rename(&part, &to).map_err(|e| format!("rename {} to {}: {e}", part.display(), to.display()))?;
            to
        }
    };
    Ok(Firmware { code, vars: committed.join(vars), flash })
}

/// `bytes` at `to`, then zeros to `flash`'s size where the machine fixes one.
fn write_padded(to: &Path, bytes: &[u8], flash: Option<u64>) -> Result<(), String> {
    let len = bytes.len() as u64;
    let size = flash.unwrap_or(len);
    if len > size {
        return Err(format!("{}: {len} bytes of firmware for a {size}-byte flash device", to.display()));
    }
    let mut file = fs::File::create(to).map_err(|e| format!("create {}: {e}", to.display()))?;
    file.write_all(bytes).map_err(|e| format!("write {}: {e}", to.display()))?;
    file.set_len(size).map_err(|e| format!("pad {} to {size} bytes: {e}", to.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A root holding `pin`'s build directory with each image named by its own
    /// name, `len` bytes.
    fn root_with(pin: &Pin, images: &[(&str, usize)]) -> toyos_tmpdir::TempDir {
        let tmp = toyos_tmpdir::TempDir::new("firmware");
        let dir = tmp.path().join(pin.dir);
        fs::create_dir_all(&dir).unwrap();
        for (name, len) in images {
            fs::write(dir.join(name), name.bytes().cycle().take(*len).collect::<Vec<u8>>()).unwrap();
        }
        tmp
    }

    #[test]
    fn every_machine_and_accelerator_names_images_the_tree_commits() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        for arch in Arch::ALL {
            for accel in [Accel::Kvm, Accel::Hvf, Accel::Tcg] {
                let Pin { dir, code, vars, .. } = *pinned(arch, accel);
                for name in [code, vars] {
                    let at = root.join(dir).join(name);
                    assert!(at.is_file(), "{arch:?} under {accel:?}: {dir}/{name} is not committed");
                }
            }
        }
    }

    #[test]
    fn virts_images_are_the_committed_bytes_then_zeros_to_the_flash() {
        let dir = VIRT.dir;
        let tmp = root_with(&VIRT, &[("QEMU_EFI.fd", 3 << 20), ("QEMU_VARS.fd", 768 << 10)]);
        let firmware = stage(tmp.path(), &VIRT).unwrap();
        assert_eq!(firmware.code, tmp.path().join("target").join(dir).join("QEMU_EFI.fd"));
        let code = fs::read(&firmware.code).unwrap();
        assert_eq!(code.len() as u64, VIRT_FLASH);
        assert_eq!(code[..3 << 20], fs::read(tmp.path().join(dir).join("QEMU_EFI.fd")).unwrap()[..]);
        assert!(code[3 << 20..].iter().all(|b| *b == 0));

        let vars = tmp.path().join("vars.fd");
        firmware.fresh_vars(&vars).unwrap();
        let store = fs::read(&vars).unwrap();
        assert_eq!(store.len() as u64, VIRT_FLASH);
        assert_eq!(store[..768 << 10], fs::read(tmp.path().join(dir).join("QEMU_VARS.fd")).unwrap()[..]);
        assert!(store[768 << 10..].iter().all(|b| *b == 0));
        let staged: Vec<_> = fs::read_dir(tmp.path().join("target").join(dir)).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(staged, ["QEMU_EFI.fd"], "the staging copy was renamed, not left beside it");
    }

    #[test]
    fn q35s_images_are_the_committed_files_as_they_are() {
        let dir = Q35.dir;
        let tmp = root_with(&Q35, &[("OVMF_CODE_4M.fd", 4096), ("OVMF_VARS_4M.fd", 512)]);
        let firmware = stage(tmp.path(), &Q35).unwrap();
        assert_eq!(firmware.code, tmp.path().join(dir).join("OVMF_CODE_4M.fd"));
        let vars = tmp.path().join("vars.fd");
        firmware.fresh_vars(&vars).unwrap();
        assert_eq!(fs::read(&vars).unwrap(), fs::read(tmp.path().join(dir).join("OVMF_VARS_4M.fd")).unwrap());
        assert!(!tmp.path().join("target").exists(), "nothing is staged for a machine that sizes its flash by the image");
    }

    #[test]
    fn an_image_larger_than_its_flash_is_refused() {
        let tmp = toyos_tmpdir::TempDir::new("firmware");
        let why = write_padded(&tmp.path().join("image.fd"), &[1, 2, 3], Some(2)).unwrap_err();
        assert!(why.ends_with("3 bytes of firmware for a 2-byte flash device"), "{why}");
    }
}
