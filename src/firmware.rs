//! The UEFI firmware a guest boots: whichever the host's QEMU installation
//! declares, never a file this tree carries or a path read off one machine.
//!
//! Found the way QEMU's interop spec (`docs/interop/firmware.json`) tells
//! management software to: the `firmware/*.json` descriptors under the user's
//! override directory, then the system's, then every data directory QEMU
//! reports (`-L help`), taken in that order, a name in an earlier directory
//! hiding the same name in a later one, and the first that fits the machine
//! wins. Nothing fits, and the boot is refused by name.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use serde::Deserialize;

use crate::arch::Arch;

/// One installation's firmware for one machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Firmware {
    /// The code image, which a guest only ever reads.
    pub code: PathBuf,
    /// The variable-store template, which no guest is handed: each boot writes
    /// a copy of its own ([`Firmware::fresh_vars`]).
    pub vars: PathBuf,
}

impl Firmware {
    /// A fresh variable store at `to`, from the template.
    pub fn fresh_vars(&self, to: &Path) -> Result<(), String> {
        std::fs::copy(&self.vars, to)
            .map_err(|e| format!("copy the firmware's variable store {} to {}: {e}", self.vars.display(), to.display()))?;
        // `fs::copy` carries the template's mode. A read-only template (a 0444
        // store, as on Nix) would leave the boot's own copy unwritable to QEMU
        // and the next boot's copy over it failing the same way.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(to, std::fs::Permissions::from_mode(0o644))
            .map_err(|e| format!("make the firmware variable store copy {} writable: {e}", to.display()))
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

/// The firmware the host's QEMU declares for `arch`'s machine, asked once per
/// process.
pub fn of(arch: Arch) -> Result<&'static Firmware, String> {
    static FOUND: [OnceLock<Result<Firmware, String>>; 2] = [OnceLock::new(), OnceLock::new()];
    let slot = &FOUND[Arch::ALL.iter().position(|a| *a == arch).expect("every Arch is in ALL")];
    slot.get_or_init(|| find(arch)).as_ref().map_err(Clone::clone)
}

fn find(arch: Arch) -> Result<Firmware, String> {
    let version = crate::ci::qemu_version(arch)?;
    let out = Command::new(arch.qemu())
        .args(["-L", "help"])
        .output()
        .map_err(|e| format!("{} -L help: {e}", arch.qemu()))?;
    if !out.status.success() {
        return Err(format!("{} -L help: {}: {}", arch.qemu(), out.status, String::from_utf8_lossy(&out.stderr)));
    }
    let datadirs = String::from_utf8_lossy(&out.stdout).lines().filter(|l| !l.is_empty()).map(|l| Path::new(l).join("firmware")).collect();
    let dirs = search_dirs(
        std::env::var_os("XDG_CONFIG_HOME").as_deref().map(Path::new),
        std::env::var_os("HOME").as_deref().map(Path::new),
        datadirs,
    );
    select(arch, &machine_type(arch, &version), &descriptors(&dirs)?).map_err(|why| {
        let searched: Vec<String> = dirs.iter().map(|d| d.display().to_string()).collect();
        format!("{why}; searched {} (QEMU {version}'s data directories among them)", searched.join(", "))
    })
}

/// The directories `descriptors` reads, in the precedence
/// `docs/interop/firmware.json` gives: the user's override
/// (`$XDG_CONFIG_HOME`, else `$HOME/.config`), then the system's, then every
/// data directory QEMU itself reports (`-L help`, `datadirs`).
fn search_dirs(xdg_config_home: Option<&Path>, home: Option<&Path>, datadirs: Vec<PathBuf>) -> Vec<PathBuf> {
    let user_config = xdg_config_home.map(Path::to_path_buf).or_else(|| home.map(|h| h.join(".config")));
    let mut dirs: Vec<PathBuf> = user_config.map(|c| c.join("qemu/firmware")).into_iter().collect();
    dirs.push(PathBuf::from("/etc/qemu/firmware"));
    dirs.extend(datadirs);
    dirs
}

/// Every `*.json` under `dirs`, read, in file-name order, a name in an earlier
/// directory hiding the same name in a later one. A directory that does not
/// exist holds none.
fn descriptors(dirs: &[PathBuf]) -> Result<Vec<(PathBuf, Vec<u8>)>, String> {
    let mut named: BTreeMap<String, PathBuf> = BTreeMap::new();
    for dir in dirs {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("{}: {e}", dir.display())),
        };
        for entry in entries {
            let path = entry.map_err(|e| format!("{}: {e}", dir.display()))?.path();
            if path.extension().is_some_and(|x| x == "json") {
                let name = path.file_name().expect("a listed file has a name").to_string_lossy().into_owned();
                named.entry(name).or_insert(path);
            }
        }
    }
    named
        .into_values()
        .map(|path| std::fs::read(&path).map(|bytes| (path.clone(), bytes)).map_err(|e| format!("{}: {e}", path.display())))
        .collect()
}

/// The versioned machine type `arch`'s machine resolves to under QEMU
/// `version`: what a descriptor's `machines` globs are matched against.
/// x86_64's PC lineage versions its type under a `pc-` prefix the `q35` alias
/// itself does not carry; aarch64's `virt` carries none.
fn machine_type(arch: Arch, version: &str) -> String {
    let release: Vec<&str> = version.split('.').take(2).collect();
    let prefix = match arch {
        Arch::X86_64 => "pc-",
        Arch::Aarch64 => "",
    };
    format!("{prefix}{}-{}", arch.machine(), release.join("."))
}

/// The first of `descriptors`, in the order given, that declares UEFI firmware
/// for `machine` on `arch` as a raw code image beside a raw variable-store
/// template, with neither secure boot nor SMM, which no guest here is set up
/// for. An empty file is one the spec says hides its name, and one that does
/// not parse is refused.
fn select(arch: Arch, machine: &str, descriptors: &[(PathBuf, Vec<u8>)]) -> Result<Firmware, String> {
    for (path, bytes) in descriptors {
        if bytes.is_empty() {
            continue;
        }
        let d: Descriptor = serde_json::from_slice(bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        let Mapping::Flash(flash) = d.mapping else { continue };
        let targets = d.targets.iter().filter(|t| t.architecture == arch.name());
        let fits_machine = targets.flat_map(|t| &t.machines).map(|g| glob(g, machine)).collect::<Result<Vec<bool>, _>>();
        let fits = d.interface_types.iter().any(|i| i == "uefi")
            && flash.mode.as_deref().is_none_or(|m| m == "split")
            && flash.executable.format == "raw"
            && flash.nvram_template.as_ref().is_some_and(|t| t.format == "raw")
            && !d.features.iter().any(|f| f == "secure-boot" || f == "requires-smm")
            && fits_machine.map_err(|why| format!("{}: {why}", path.display()))?.contains(&true);
        if fits {
            return Ok(Firmware {
                code: flash.executable.filename,
                vars: flash.nvram_template.expect("a fitting descriptor names its template").filename,
            });
        }
    }
    let read: Vec<String> = descriptors.iter().map(|(p, _)| p.display().to_string()).collect();
    Err(format!(
        "no firmware descriptor declares UEFI flash firmware without secure boot for {} `{machine}`; read [{}]",
        arch.name(),
        read.join(", ")
    ))
}

/// Whether `pattern` matches `name`. Anything else — a non-trailing `*`, or
/// another fnmatch metacharacter — is refused by name rather than misread.
fn glob(pattern: &str, name: &str) -> Result<bool, String> {
    match pattern.strip_suffix('*') {
        Some(prefix) if !prefix.contains(['*', '?', '[', '\\']) => Ok(name.starts_with(prefix)),
        None if !pattern.contains(['?', '[', '\\']) => Ok(pattern == name),
        _ => Err(format!("the machine glob {pattern:?} is not a prefix with at most one trailing `*`, which this reader does not match")),
    }
}

/// The fields of `docs/interop/firmware.json`'s `Firmware` this reader decides by.
#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Descriptor {
    interface_types: Vec<String>,
    mapping: Mapping,
    targets: Vec<Target>,
    features: Vec<String>,
}

#[derive(Deserialize)]
#[serde(tag = "device", rename_all = "kebab-case")]
enum Mapping {
    Flash(Flash),
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Flash {
    mode: Option<String>,
    executable: File,
    nvram_template: Option<File>,
}

#[derive(Deserialize)]
struct File {
    filename: PathBuf,
    format: String,
}

#[derive(Deserialize)]
struct Target {
    architecture: String,
    machines: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(arch: &str, machines: &str, features: &str, code: &str) -> Vec<u8> {
        format!(
            r#"{{"description":"t","interface-types":["uefi"],
               "mapping":{{"device":"flash","executable":{{"filename":"{code}","format":"raw"}},
                           "nvram-template":{{"filename":"{code}.vars","format":"raw"}}}},
               "targets":[{{"architecture":"{arch}","machines":[{machines}]}}],
               "features":[{features}],"tags":[]}}"#
        )
        .into_bytes()
    }

    fn named(files: &[(&str, Vec<u8>)]) -> Vec<(PathBuf, Vec<u8>)> {
        files.iter().map(|(n, b)| (PathBuf::from(*n), b.clone())).collect()
    }

    #[test]
    fn the_first_non_secure_uefi_flash_descriptor_for_the_machine_wins() {
        let files = named(&[
            ("40-memory.json", br#"{"interface-types":["uefi"],"mapping":{"device":"memory","filename":"/sev.fd"},"targets":[{"architecture":"x86_64","machines":["pc-q35-*"]}],"features":["amd-sev"]}"#.to_vec()),
            ("50-secure.json", descriptor("x86_64", r#""pc-q35-*""#, r#""requires-smm","secure-boot""#, "/secure.fd")),
            ("55-i440fx.json", descriptor("x86_64", r#""pc-i440fx-*""#, "", "/i440fx.fd")),
            ("56-arm.json", descriptor("aarch64", r#""virt-*""#, "", "/arm.fd")),
            ("58-hidden.json", Vec::new()),
            // Fedora-style non-secure descriptors ahead of the raw ones: each
            // fits every rule but the one it is named for, and must stay
            // refused by that rule alone.
            (
                "58a-qcow2.json",
                br#"{"interface-types":["uefi"],"mapping":{"device":"flash","executable":{"filename":"/qcow2.fd","format":"qcow2"},"nvram-template":{"filename":"/qcow2.fd.vars","format":"raw"}},"targets":[{"architecture":"x86_64","machines":["pc-q35-*"]}],"features":[]}"#.to_vec(),
            ),
            (
                "58b-smm.json",
                descriptor("x86_64", r#""pc-q35-*""#, r#""requires-smm""#, "/smm.fd"),
            ),
            (
                "59-combined.json",
                br#"{"interface-types":["uefi"],"mapping":{"device":"flash","mode":"combined","executable":{"filename":"/combined.fd","format":"raw"},"nvram-template":{"filename":"/combined.fd.vars","format":"raw"}},"targets":[{"architecture":"x86_64","machines":["pc-q35-*"]}],"features":[]}"#.to_vec(),
            ),
            ("60-plain.json", descriptor("x86_64", r#""pc-i440fx-*","pc-q35-*""#, r#""acpi-s3","amd-sev""#, "/plain.fd")),
            ("70-later.json", descriptor("x86_64", r#""pc-q35-*""#, "", "/later.fd")),
        ]);
        assert_eq!(
            select(Arch::X86_64, "pc-q35-11.1", &files),
            Ok(Firmware {
                code: PathBuf::from("/plain.fd"),
                vars: PathBuf::from("/plain.fd.vars"),
            })
        );
        assert_eq!(select(Arch::Aarch64, "virt-11.1", &files).map(|f| f.code), Ok(PathBuf::from("/arm.fd")));
    }

    #[test]
    fn nothing_fitting_is_refused_naming_what_was_read() {
        let files = named(&[("50-secure.json", descriptor("x86_64", r#""pc-q35-*""#, r#""secure-boot""#, "/s.fd"))]);
        let why = select(Arch::X86_64, "pc-q35-11.1", &files).unwrap_err();
        assert!(why.contains("x86_64 `pc-q35-11.1`") && why.contains("50-secure.json"), "{why}");
        assert!(select(Arch::X86_64, "pc-q35-11.1", &[]).is_err());
    }

    #[test]
    fn a_descriptor_that_does_not_parse_is_refused_by_name() {
        let files = named(&[("10-broken.json", b"{".to_vec()), ("60-plain.json", descriptor("x86_64", r#""pc-q35-*""#, "", "/p.fd"))]);
        let why = select(Arch::X86_64, "pc-q35-11.1", &files).unwrap_err();
        assert!(why.starts_with("10-broken.json: "), "{why}");
    }

    #[test]
    fn an_earlier_directory_hides_a_later_ones_name_and_a_missing_one_holds_nothing() {
        let tmp = toyos_tmpdir::TempDir::new("firmware-descriptors");
        let [first, second] = ["first", "second"].map(|d| tmp.path().join(d));
        for (dir, files) in [(&first, &["60-b.json"][..]), (&second, &["60-b.json", "50-a.json", "README"][..])] {
            std::fs::create_dir(dir).unwrap();
            for file in files {
                std::fs::write(dir.join(file), dir.display().to_string()).unwrap();
            }
        }
        let read = descriptors(&[tmp.path().join("absent"), first.clone(), second.clone()]).unwrap();
        assert_eq!(
            read,
            vec![
                (second.join("50-a.json"), second.display().to_string().into_bytes()),
                (first.join("60-b.json"), first.display().to_string().into_bytes()),
            ]
        );
    }

    #[test]
    fn the_machine_is_the_versioned_type_its_alias_resolves_to() {
        assert_eq!(machine_type(Arch::X86_64, "11.1.1"), "pc-q35-11.1");
        assert_eq!(machine_type(Arch::Aarch64, "11.1.1"), "virt-11.1");
    }

    #[test]
    fn a_glob_matches_a_trailing_star_as_a_prefix_and_refuses_anything_else() {
        assert_eq!(glob("pc-q35-*", "pc-q35-11.1"), Ok(true));
        assert_eq!(glob("pc-q35-*", "pc-i440fx-11.1"), Ok(false));
        assert_eq!(glob("pc-q35-11.0", "pc-q35-11.1"), Ok(false));
        assert_eq!(glob("pc-q35-11.1", "pc-q35-11.1"), Ok(true));
        assert_eq!(glob("*", "virt-11.1"), Ok(true));
        assert!(glob("pc-*-11.*", "pc-q35-11.1").is_err());
        assert!(glob("pc-q35-1?.*", "pc-q35-11.1").is_err());
    }

    #[test]
    fn the_users_and_the_systems_directories_come_before_qemus_own() {
        let dirs = search_dirs(Some(Path::new("/x/cfg")), Some(Path::new("/x/home")), vec![PathBuf::from("/data/firmware")]);
        assert_eq!(
            dirs,
            vec![PathBuf::from("/x/cfg/qemu/firmware"), PathBuf::from("/etc/qemu/firmware"), PathBuf::from("/data/firmware")]
        );
        let dirs = search_dirs(None, Some(Path::new("/x/home")), vec![]);
        assert_eq!(dirs[0], PathBuf::from("/x/home/.config/qemu/firmware"));
        let dirs = search_dirs(None, None, vec![PathBuf::from("/data/firmware")]);
        assert_eq!(dirs, vec![PathBuf::from("/etc/qemu/firmware"), PathBuf::from("/data/firmware")]);
    }

    #[test]
    fn a_read_only_templates_copy_is_still_writable() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = toyos_tmpdir::TempDir::new("firmware-vars");
        let template = tmp.path().join("template.fd");
        std::fs::write(&template, b"vars").unwrap();
        std::fs::set_permissions(&template, std::fs::Permissions::from_mode(0o444)).unwrap();
        let firmware = Firmware { code: PathBuf::new(), vars: template };
        let to = tmp.path().join("copy.fd");
        firmware.fresh_vars(&to).unwrap();
        let mode = std::fs::metadata(&to).unwrap().permissions().mode() & 0o777;
        assert_ne!(mode & 0o200, 0, "the copy must be owner-writable: {mode:04o}");
        // A second boot copies over the same file: still possible only because
        // the first copy did not inherit the template's read-only mode.
        firmware.fresh_vars(&to).unwrap();
    }
}
