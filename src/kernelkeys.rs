//! Whose keys a kernel hash container holds.
//!
//! **The scan is gone and the compiler holds its rule**: `kernel/src/hasher.rs`
//! is the only `BuildHasher` a container can name, so every spelling closes at
//! once where a text scan closed four forms and walked past four more.
//!
//! **[`DECLARED`] stays, because the origin of a key is what no compiler holds.**
//! A container whose keys crossed the boundary is a collision flood away from a
//! linear probe under whatever lock it sits behind — a seed the flooder cannot
//! read raises the price of finding one and does not bound the worst case — so
//! it is a `BTreeMap`/`BTreeSet`. This table is what is left hashed.
//!
//! **A row's `keys` sentence is a human assertion and nothing checks it**: the
//! origin of a key is a whole-program question, so **whoever reviews a new row
//! owes the trace of that key across the boundary** — and there is no longer a
//! red here that adding a row is the cheapest way to close.

/// One hashed container the kernel keeps, and why its keys cannot be chosen
/// across the boundary.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every row says something about where its keys come from: a row with an
    /// empty reason is a declaration that decided nothing.
    #[test]
    fn every_declared_row_names_the_minter_of_its_keys() {
        for row in DECLARED {
            assert!(!row.keys.trim().is_empty(), "{} / {} says nothing", row.file, row.ty);
        }
    }

    /// A row names a file the tree holds, and the hasher's module exists. Not a
    /// scan: a path nobody moved when the code moved is a row about nothing.
    #[test]
    fn every_declared_row_names_a_file_that_exists() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        assert!(
            root.join("kernel/src/hasher.rs").is_file(),
            "the hasher this table's containers are built on is what replaced the scan"
        );
        for row in DECLARED {
            assert!(
                root.join(row.file).is_file(),
                "{} is declared to hold {} and no such file exists",
                row.file,
                row.ty
            );
        }
    }
}
