//! The UEFI firmware a guest boots: Debian's edk2 2026.05-2 build, committed
//! under [`DIR`] and recorded in `NOTICE`. It is the half of the instrument
//! `.github/qemu-version` does not declare, so the dev host and CI boot the
//! same bytes, and no guest boots what the host's QEMU installation carries.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::arch::Arch;

/// Where the build is committed, under the repository's root.
const DIR: &str = "qemu-firmware/debian-2026.05-2";

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

/// `arch`'s firmware, staged once per process.
pub fn of(arch: Arch) -> Result<&'static Firmware, String> {
    static STAGED: [OnceLock<Result<Firmware, String>>; 2] = [OnceLock::new(), OnceLock::new()];
    let slot = &STAGED[Arch::ALL.iter().position(|a| *a == arch).expect("every Arch is in ALL")];
    slot.get_or_init(|| stage(Path::new(env!("CARGO_MANIFEST_DIR")), arch)).as_ref().map_err(Clone::clone)
}

/// The committed images for `arch`, and for a machine whose flash is fixed, the
/// code image padded to it under `root`'s `target/`: written beside the last
/// copy and renamed over it, so a guest that opened that one reads it whole.
fn stage(root: &Path, arch: Arch) -> Result<Firmware, String> {
    let (code, vars, flash) = match arch {
        Arch::X86_64 => ("OVMF_CODE_4M.fd", "OVMF_VARS_4M.fd", None),
        Arch::Aarch64 => ("QEMU_EFI.fd", "QEMU_VARS.fd", Some(VIRT_FLASH)),
    };
    let committed = root.join(DIR);
    let code = match flash {
        None => committed.join(code),
        Some(_) => {
            let bytes = fs::read(committed.join(code))
                .map_err(|e| format!("read the firmware {}: {e}", committed.join(code).display()))?;
            let dir = root.join("target").join(DIR);
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

    /// A root holding `DIR` with each image named by its own name, `len` bytes.
    fn root_with(images: &[(&str, usize)]) -> toyos_tmpdir::TempDir {
        let tmp = toyos_tmpdir::TempDir::new("firmware");
        let dir = tmp.path().join(DIR);
        fs::create_dir_all(&dir).unwrap();
        for (name, len) in images {
            fs::write(dir.join(name), name.bytes().cycle().take(*len).collect::<Vec<u8>>()).unwrap();
        }
        tmp
    }

    #[test]
    fn every_machine_names_images_the_tree_commits() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        for arch in Arch::ALL {
            let names = match arch {
                Arch::X86_64 => ["OVMF_CODE_4M.fd", "OVMF_VARS_4M.fd"],
                Arch::Aarch64 => ["QEMU_EFI.fd", "QEMU_VARS.fd"],
            };
            for name in names {
                assert!(root.join(DIR).join(name).is_file(), "{arch:?}: {DIR}/{name} is not committed");
            }
        }
    }

    #[test]
    fn virts_images_are_the_committed_bytes_then_zeros_to_the_flash() {
        let tmp = root_with(&[("QEMU_EFI.fd", 3 << 20), ("QEMU_VARS.fd", 768 << 10)]);
        let firmware = stage(tmp.path(), Arch::Aarch64).unwrap();
        assert_eq!(firmware.code, tmp.path().join("target").join(DIR).join("QEMU_EFI.fd"));
        let code = fs::read(&firmware.code).unwrap();
        assert_eq!(code.len() as u64, VIRT_FLASH);
        assert_eq!(code[..3 << 20], fs::read(tmp.path().join(DIR).join("QEMU_EFI.fd")).unwrap()[..]);
        assert!(code[3 << 20..].iter().all(|b| *b == 0));

        let vars = tmp.path().join("vars.fd");
        firmware.fresh_vars(&vars).unwrap();
        let store = fs::read(&vars).unwrap();
        assert_eq!(store.len() as u64, VIRT_FLASH);
        assert_eq!(store[..768 << 10], fs::read(tmp.path().join(DIR).join("QEMU_VARS.fd")).unwrap()[..]);
        assert!(store[768 << 10..].iter().all(|b| *b == 0));
        let staged: Vec<_> = fs::read_dir(tmp.path().join("target").join(DIR)).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(staged, ["QEMU_EFI.fd"], "the staging copy was renamed, not left beside it");
    }

    #[test]
    fn q35s_images_are_the_committed_files_as_they_are() {
        let tmp = root_with(&[("OVMF_CODE_4M.fd", 4096), ("OVMF_VARS_4M.fd", 512)]);
        let firmware = stage(tmp.path(), Arch::X86_64).unwrap();
        assert_eq!(firmware.code, tmp.path().join(DIR).join("OVMF_CODE_4M.fd"));
        let vars = tmp.path().join("vars.fd");
        firmware.fresh_vars(&vars).unwrap();
        assert_eq!(fs::read(&vars).unwrap(), fs::read(tmp.path().join(DIR).join("OVMF_VARS_4M.fd")).unwrap());
        assert!(!tmp.path().join("target").exists(), "nothing is staged for a machine that sizes its flash by the image");
    }

    #[test]
    fn an_image_larger_than_its_flash_is_refused() {
        let tmp = toyos_tmpdir::TempDir::new("firmware");
        let why = write_padded(&tmp.path().join("image.fd"), &[1, 2, 3], Some(2)).unwrap_err();
        assert!(why.ends_with("3 bytes of firmware for a 2-byte flash device"), "{why}");
    }
}
