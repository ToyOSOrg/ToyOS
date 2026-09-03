//! Whose keys a kernel hash container holds.
//!
//! **The scan is gone and the compiler holds its rule.** `kernel/Cargo.toml`
//! takes hashbrown without `default-hasher`, so `DefaultHashBuilder` has no
//! `BuildHasher` impl and neither `HashMap::new` nor `HashMap::with_capacity`
//! exists; every container in `kernel/src` names `kernel/src/hasher.rs`'s
//! `KernelHashState`, seeded once from `RDRAND` before any of them is built.
//! That closes every spelling at once — an import alias, a turbofish, a type
//! inferred from its constructor — where the text scan that stood here closed
//! four measured forms and walked past four more.
//!
//! **[`DECLARED`] stays, because the origin of a key is what no compiler holds.**
//! A container whose keys crossed the user/kernel boundary is a collision flood
//! away from a linear probe under whatever lock it sits behind — a seed the
//! flooder cannot read raises the price of finding one and does not bound the
//! worst case — so it is a `BTreeMap`/`BTreeSet`: logarithmic whatever the keys
//! are. This table is what is left hashed, and every row says who mints its
//! keys.
//!
//! **A row's `keys` sentence is a human assertion and nothing checks it.** The
//! origin of a key is a whole-program question; the only test over that column
//! asks that it is not empty. So **whoever reviews a new row owes the trace of
//! that key across the boundary** — and there is no longer a red here that
//! adding a row is the cheapest way to close.

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

    /// A row names a file the tree holds, and names the hasher's own module
    /// once. Not a scan over the sources — the compiler holds the containers —
    /// but a path nobody moved when the code moved is a row about nothing.
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
