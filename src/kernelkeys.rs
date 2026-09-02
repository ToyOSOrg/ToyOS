//! Whose keys a kernel hash container may hold.
//!
//! `kernel/Cargo.toml` takes hashbrown's `default-hasher`, so every
//! `HashMap`/`HashSet` the kernel builds gets `foldhash::fast::RandomState`,
//! whose two seeds are addresses and nothing else — and this machine has no
//! address space layout randomisation for that to lean on, so both seeds are
//! the same on every boot of the same image. A container whose keys crossed the
//! user/kernel boundary is therefore a collision flood away from a linear probe
//! under whatever lock it sits behind. The answer is unrepresentable rather than
//! checked: such a container is a `BTreeMap`/`BTreeSet`, logarithmic in the
//! worst case with no seed to derive. [`DECLARED`] is what is left hashed, and
//! every row says where its keys are minted.
//!
//! **This is a text scan, and it closes exactly one spelling**: a `HashMap<…>`
//! or `HashSet<…>` written with its generic arguments, on a line that is not a
//! comment, in a `.rs` file under `kernel/src`. It does not reach a `type`
//! alias, a re-export, a map whose type is inferred from `HashMap::new()`, or
//! any other hashed container. **The exit condition is the compiler**: dropping
//! `default-hasher` from `kernel/Cargo.toml` deletes `DefaultHashBuilder`'s
//! `BuildHasher` impl, so no `HashMap::new()` compiles until it names a
//! `BuildHasher` the kernel seeds itself — every spelling at once, and this scan
//! goes with it.

use std::collections::BTreeMap;
use std::path::Path;

/// One hashed container the kernel is allowed to keep, and why its keys cannot
/// be chosen across the boundary.
pub struct Declared {
    /// Path under the repository root.
    pub file: &'static str,
    /// The type as it is written at the site, generic arguments included.
    pub ty: &'static str,
    /// Where the key is minted. A key userland can choose is not a row here; it
    /// is a `BTreeMap`.
    pub keys: &'static str,
}

/// Every hashed container in `kernel/src`, with the origin of its keys.
pub const DECLARED: &[Declared] = &[
    Declared {
        file: "kernel/src/bcachefs_adapter.rs",
        ty: "HashMap<FileId, OpenFileInfo>",
        keys: "`file_cache::FileId`, minted by the file cache",
    },
    Declared {
        file: "kernel/src/fat32_adapter.rs",
        ty: "HashMap<FileId, OpenFile>",
        keys: "`file_cache::FileId`, minted by the file cache",
    },
    Declared {
        file: "kernel/src/id_map.rs",
        ty: "HashMap<K, V>",
        keys: "`IdKey`, which no integer implements: every key is an id this kernel issued",
    },
    Declared {
        file: "kernel/src/mm/paging.rs",
        ty: "HashMap<u64, super::pmm::PhysPage>",
        keys: "a physical address the page allocator returned",
    },
    Declared {
        file: "kernel/src/page_cache.rs",
        ty: "HashMap<u64, u32>",
        keys: "a device block number, bounded by the device and by the cache's own slot count",
    },
    Declared {
        file: "kernel/src/scheduler.rs",
        ty: "HashMap<Pid, Arc<KShare>>",
        keys: "`process::Pid`, minted by the process table",
    },
    Declared {
        file: "kernel/src/vfs.rs",
        ty: "HashMap<String, Mount>",
        keys: "a mount name, passed by `main.rs` alone — no syscall mounts anything",
    },
];

/// Every `HashMap<…>`/`HashSet<…>` this scan can see, as `file -> types`.
///
/// A comment line is skipped, and a `HashMap` written without its generic
/// arguments has no type to read and is not one of them.
fn found(root: &Path) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut stack = vec![root.join("kernel/src")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("kernel/src is readable") {
            let path = entry.expect("a directory entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("a kernel source file");
            let rel = path
                .strip_prefix(root)
                .expect("under the repository root")
                .to_string_lossy()
                .replace('\\', "/");
            for line in text.lines() {
                if line.trim_start().starts_with("//") {
                    continue;
                }
                for ty in types_in(line) {
                    out.entry(rel.clone()).or_default().push(ty);
                }
            }
        }
    }
    for types in out.values_mut() {
        types.sort();
    }
    out
}

/// Every `HashMap<…>`/`HashSet<…>` in one line, read to its balanced `>`.
fn types_in(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    for name in ["HashMap", "HashSet"] {
        let mut from = 0;
        while let Some(at) = line[from..].find(name).map(|p| p + from) {
            from = at + name.len();
            let rest = &line[from..];
            if !rest.starts_with('<') {
                continue;
            }
            let mut depth = 0usize;
            let mut end = None;
            for (i, c) in rest.char_indices() {
                match c {
                    '<' => depth += 1,
                    '>' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(i + 1);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if let Some(end) = end {
                out.push(format!("{name}{}", &rest[..end]));
            }
        }
    }
    out
}

/// Refusals for a tree that does not match [`DECLARED`], as whole sentences.
pub fn refusals(root: &Path) -> Vec<String> {
    let mut declared: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for row in DECLARED {
        declared.entry(row.file).or_default().push(row.ty);
    }
    for types in declared.values_mut() {
        types.sort();
    }

    let found = found(root);
    let mut out = Vec::new();
    for (file, types) in &found {
        let want = declared.get(file.as_str());
        match want {
            Some(want) if *want == types.iter().map(String::as_str).collect::<Vec<_>>() => {}
            Some(want) => out.push(format!(
                "{file} holds {types:?}; src/kernelkeys.rs declares {want:?}. A container keyed by \
                 anything userland chooses is a BTreeMap/BTreeSet, not a row here"
            )),
            None => out.push(format!(
                "{file} holds {types:?} and src/kernelkeys.rs declares no hashed container there. \
                 A container keyed by anything userland chooses is a BTreeMap/BTreeSet"
            )),
        }
    }
    for file in declared.keys() {
        if !found.contains_key(*file) {
            out.push(format!(
                "src/kernelkeys.rs declares a hashed container in {file} and the file holds none — \
                 retire the row"
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR"))
    }

    #[test]
    fn every_hashed_kernel_container_is_declared_with_a_kernel_minted_key() {
        let refusals = refusals(root());
        assert!(refusals.is_empty(), "{}", refusals.join("\n  "));
    }

    /// Every row says something about where its keys come from: a row with an
    /// empty reason is a declaration that decided nothing.
    #[test]
    fn every_declared_row_names_the_minter_of_its_keys() {
        for row in DECLARED {
            assert!(!row.keys.trim().is_empty(), "{} / {} says nothing", row.file, row.ty);
        }
    }

    /// Teeth: the scan's own reading of a line, over the shapes the tree holds
    /// and the one the fix removed.
    #[test]
    fn the_scan_reads_a_nested_generic_to_its_balanced_close() {
        assert_eq!(types_in("    mounts: HashMap<String, Mount>,"), ["HashMap<String, Mount>"]);
        assert_eq!(
            types_in("    blocks: HashMap<String, Weak<FileBlocks>>,"),
            ["HashMap<String, Weak<FileBlocks>>"]
        );
        assert_eq!(
            types_in("static S: Lock<Option<HashMap<Pid, Arc<KShare>>>> = x;"),
            ["HashMap<Pid, Arc<KShare>>"]
        );
        assert_eq!(types_in("    created_dirs: HashSet<String>,"), ["HashSet<String>"]);
        // No generic arguments, so no type to read — the spelling this scan
        // does not close, stated as a test rather than only as prose.
        assert!(types_in("    let mut seen = HashSet::new();").is_empty());
    }
}
