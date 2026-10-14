//! The microcode an image carries: every vendor file pinned under
//! `build/microcode/` (`NOTICE`), validated whole and split into one patch
//! per CPU, each written on ROOT where `toyos_microcode` names it; the loader
//! hands the kernel its CPU's. An image carries the patches its [`Machines`]
//! list names in `images/microcode.toml`, and a QEMU image none.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

pub mod amd;
pub mod intel;

#[cfg(test)]
mod tests;

/// Intel's files, each Intel's updates for one processor signature.
const INTEL: [(&str, &[u8]); 2] = [
    ("intel-ucode/06-8c-01", include_bytes!("microcode/intel-ucode/06-8c-01")),
    ("intel-ucode/06-cc-02", include_bytes!("microcode/intel-ucode/06-cc-02")),
];

/// AMD's containers, each AMD's patches for one family.
const AMD: [(&str, &[u8]); 1] =
    [("amd-ucode/microcode_amd_fam17h.bin", include_bytes!("microcode/amd-ucode/microcode_amd_fam17h.bin"))];

/// Where the lists of patches an image carries are, under the repository root.
pub const LISTS: &str = "images/microcode.toml";

/// The machines an image is built for, which decide the patches it carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Machines {
    /// A QEMU guest, whose microcode is the hypervisor's: none.
    Qemu,
    /// The benches the metal harness flashes.
    Benches,
    /// Every machine an image built to leave this host is for.
    Release,
}

/// `images/microcode.toml`: the patches each kind of image carries, by name.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Lists {
    benches: Vec<String>,
    release: Vec<String>,
}

/// One patch as a CPU loads it: the level its header names, and its bytes in
/// its vendor's own form, which the file on ROOT holds whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Patch<'a> {
    pub revision: u32,
    pub bytes: &'a [u8],
}

/// Why the pinned files split into no set of patches.
#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    Intel { file: &'static str, refused: intel::Refused },
    Amd { file: &'static str, refused: amd::Refused },
    /// Two different patches name one CPU at the newest level any names it.
    Tie { name: String, revision: u32 },
    /// Two different signatures, or an Intel one on two platforms, take one
    /// name: the bits a name leaves out tell them apart.
    Collision { name: String },
}

/// Every patch the pinned files hold, by the name the loader looks it up by.
pub fn split() -> Result<BTreeMap<String, Patch<'static>>, Refused> {
    let mut named = Vec::new();
    for (file, bytes) in INTEL {
        for update in intel::updates(bytes).map_err(|refused| Refused::Intel { file, refused })? {
            named.extend(update.patches());
        }
    }
    for (file, bytes) in AMD {
        named.extend(amd::patches(bytes).map_err(|refused| Refused::Amd { file, refused })?);
    }
    newest(named_cpus(named.into_iter())?.into_iter())
}

/// Each patch under its CPU's name, refusing two CPUs under one.
fn named_cpus<'a>(
    named: impl Iterator<Item = (toyos_microcode::Cpu, Patch<'a>)>,
) -> Result<Vec<(String, Patch<'a>)>, Refused> {
    let mut cpus: BTreeMap<String, toyos_microcode::Cpu> = BTreeMap::new();
    named
        .map(|(cpu, patch)| {
            let name = cpu.to_string();
            match *cpus.entry(name.clone()).or_insert(cpu) == cpu {
                true => Ok((name, patch)),
                false => Err(Refused::Collision { name }),
            }
        })
        .collect()
}

/// For each name, the patch of the highest level naming it. Two different
/// patches at that level choose nothing between them; one patch named twice,
/// as by two files that both carry it, is one.
fn newest<'a>(named: impl Iterator<Item = (String, Patch<'a>)>) -> Result<BTreeMap<String, Patch<'a>>, Refused> {
    let mut newest: BTreeMap<String, (Patch<'a>, bool)> = BTreeMap::new();
    for (name, patch) in named {
        let entry = newest.entry(name).or_insert((patch, false));
        match (patch.revision.cmp(&entry.0.revision), patch == entry.0) {
            (std::cmp::Ordering::Greater, _) => *entry = (patch, false),
            (std::cmp::Ordering::Equal, false) => entry.1 = true,
            _ => {}
        }
    }
    newest
        .into_iter()
        .map(|(name, (patch, tied))| match tied {
            false => Ok((name, patch)),
            true => Err(Refused::Tie { name, revision: patch.revision }),
        })
        .collect()
}

/// The files `machines` carries on ROOT, where `toyos_microcode` puts them.
pub fn root_files(root: &Path, machines: Machines) -> Vec<(String, Vec<u8>)> {
    let names = match machines {
        Machines::Qemu => return Vec::new(),
        Machines::Benches => lists(root).benches,
        Machines::Release => lists(root).release,
    };
    let patches = split().unwrap_or_else(|refused| panic!("the pinned microcode is refused: {refused:?}"));
    names
        .into_iter()
        .map(|name| match patches.get(&name) {
            Some(patch) => (format!("{}/{name}", toyos_microcode::DIR), patch.bytes.to_vec()),
            None => panic!("{LISTS} names {name}, and no pinned microcode file holds a patch of that name"),
        })
        .collect()
}

fn lists(root: &Path) -> Lists {
    let path = root.join(LISTS);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    toml::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}
