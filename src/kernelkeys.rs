//! Whose keys a kernel hash container may hold.
//!
//! A container whose keys crossed the boundary is a collision flood away from a
//! linear probe under whatever lock it sits behind, and the kernel's seed is no
//! defence against that (`kernel/src/hasher.rs` says why), so it is a
//! `BTreeMap`/`BTreeSet`: logarithmic worst case, no seed at all.
//! [`DECLARED`] is what is left hashed, and every row says who mints its keys.
//! The scan below is what holds *whether every container is declared*; nothing
//! in the compiler answers that.
//!
//! **A row's `keys` sentence is a human assertion and nothing checks it**: the
//! scan matches types, not origins, and the only test over that column asks
//! that it is not empty. A false one greens the gate — measured, with
//! `created_dirs` back as a `HashSet` and a row calling its key kernel-minted —
//! and adding a row is the cheapest way to close a red here, so **whoever
//! reviews a new row owes the trace of that key across the boundary**.
//!
//! **This is a text scan closing one spelling**: a `HashMap<…>`/`HashSet<…>`
//! with its generic arguments, balanced on one line, not a comment, under
//! `kernel/src`. A `type` alias's *definition* is caught, since it writes those
//! arguments. Four forms walk past, each measured and each asserted in
//! [`tests::the_scan_walks_past_four_measured_forms`]: an import alias
//! (`use hashbrown::HashSet as UserKeyed;`, which compiles and builds an image);
//! arguments split across lines, as rustfmt writes a long type; a turbofish
//! (`HashSet::<String>::new()`, *written* and not inferred); and a type that is
//! inferred, from `HashMap::new()`. Those four are why the enumeration is a
//! floor and not a proof, and the exit condition for them is a parsed walk.

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
    /// is a `BTreeMap`. Nothing checks this sentence — see the module header.
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
        ty: "HashMap<BlockKey, u32>",
        keys: "`block::BlockKey`, minted only by a `Partition` this kernel opened and bounded by that view",
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

/// The one file the scan does not read, by exact path: it *defines* the alias
/// the others name, over constants. An entry here is a reviewed edit, never a
/// marker a file gives itself.
const DEFINES_THE_ALIAS: &str = "kernel/src/hasher.rs";

/// Every `HashMap<…>`/`HashSet<…>` this scan can see, as `file -> types`.
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
            if rel == DEFINES_THE_ALIAS {
                continue;
            }
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
        // A `type` alias *definition* writes its arguments, so it is caught —
        // the header used to claim the opposite.
        assert_eq!(
            types_in("type UserKeyed = hashbrown::HashSet<String>;"),
            ["HashSet<String>"]
        );
    }

    /// The four forms that walk past, each one measured against a kernel that
    /// compiled — the header names the same four and neither may drift from the
    /// other. Asserted rather than only written down, because a limitation
    /// nothing executes is the unchecked sentence this gate exists to replace.
    #[test]
    fn the_scan_walks_past_four_measured_forms() {
        // 1. An import alias: the `use` carries no arguments, and the field it
        //    renames then carries neither word.
        assert!(types_in("use hashbrown::HashSet as UserKeyed;").is_empty());
        assert!(types_in("    created_dirs: UserKeyed<String>,").is_empty());
        // 2. Arguments split across lines, which is what rustfmt produces for a
        //    long type: no balanced `>` on the line the name is on.
        assert!(types_in("    extents: HashMap<").is_empty());
        assert!(types_in("        String,").is_empty());
        assert!(types_in("        Weak<FatExtents>,").is_empty());
        assert!(types_in("    >,").is_empty());
        // 3. A turbofish, which is written and not inferred: `::<` is not `<`.
        assert!(types_in("    let mut seen = HashSet::<String>::new();").is_empty());
        // 4. A type that really is inferred from its constructor.
        assert!(types_in("    let mut seen = HashSet::new();").is_empty());
    }
}
