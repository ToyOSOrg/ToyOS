//! The C sysroot: what the ToyOS clang reads through `--sysroot` —
//! `include/` and `lib/libtoyos_c.a` for one guest target, made from
//! `userland/libc`.
//!
//! **Its `libtoyos_c.a` is libc's `staticlib`, and not the Rust sysroot's
//! archive of the same name.** That one (`src/libc.rs`) is libc with
//! `std-runtime`, linked into std, and carries no entry, allocator or panic
//! handler because std brings its own. A C program has no Rust crate to bring
//! them, and the `staticlib` carries all three — `_start` among them, which is
//! why the driver needs no start file — and the compiler's runtime builtins
//! with them.
//!
//! **Content-addressed**: `target/toyos-c/<hash>/<target>/`, the hash of every
//! header and the archive's bytes. A directory is written once, under a name of
//! its own, and renamed into place whole, so builds and tests in parallel share
//! one with no lock, and one that exists is complete.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use sha2::{Digest, Sha256};

use crate::arch::Arch;
use crate::buildlock;

/// The crate a C sysroot is made from.
const LIBC: &str = "userland/libc";

/// A C sysroot, and the clang that reads it.
#[derive(Clone, Debug)]
pub struct CSysroot {
    /// What `--sysroot` names: `include/` and `lib/`.
    pub dir: PathBuf,
    /// The clang of the toolchain the archive was built with.
    pub clang: PathBuf,
    /// That toolchain's archiver.
    pub ar: PathBuf,
    /// That toolchain's ELF reader.
    pub readobj: PathBuf,
    /// The guest target, as clang's `--target` spells it.
    pub target: &'static str,
}

impl CSysroot {
    /// The arguments every compile and link against this sysroot starts with.
    pub fn args(&self) -> [String; 2] {
        [format!("--target={}", self.target), format!("--sysroot={}", self.dir.display())]
    }

    /// What names this C toolchain to the `cc` crate for this target, as the
    /// variables it reads: so a crate that compiles C for ToyOS — doom's
    /// doomgeneric, and any other — builds it with this clang against this
    /// sysroot and archives it with this `llvm-ar`, and never with the host's.
    pub fn cc_env(&self) -> Vec<(String, String)> {
        let suffix = self.target.replace('-', "_");
        vec![
            (format!("CC_{suffix}"), self.clang.display().to_string()),
            (format!("AR_{suffix}"), self.ar.display().to_string()),
            (format!("CFLAGS_{suffix}"), format!("--sysroot={}", self.dir.display())),
        ]
    }
}

/// The C sysroot of the toolchain at `toolchain` for `arch`, made if nobody has.
///
/// Once per process for each: cargo decides whether the archive is stale, and
/// nothing in one process moves the sources it would decide from. The archive
/// is built and read under [`buildlock::artifact`], as every other guest
/// artifact is, so a second suite's cargo cannot rewrite it under this one's
/// read; the caller must not hold that lock.
pub fn ensure(root: &Path, toolchain: &Path, arch: Arch) -> CSysroot {
    static MADE: Mutex<BTreeMap<(PathBuf, Arch), CSysroot>> = Mutex::new(BTreeMap::new());
    let key = (toolchain.to_path_buf(), arch);
    if let Some(made) = MADE.lock().expect("a C sysroot build panicked").get(&key) {
        return made.clone();
    }
    let _artifact = buildlock::artifact(root);
    let mut made = MADE.lock().expect("a C sysroot build panicked");
    made.entry(key)
        .or_insert_with(|| {
            let target = arch.userland();
            let archive = build_archive(root, toolchain, target);
            let dir = place(root, &root.join(LIBC).join("include"), &archive, target);
            CSysroot {
                dir,
                clang: crate::clang::clang(toolchain),
                ar: crate::clang::ar(toolchain),
                readobj: crate::clang::readobj(toolchain),
                target,
            }
        })
        .clone()
}

/// Build libc as a `staticlib` for `target` with the toolchain at
/// `toolchain`, and return the archive's path.
///
/// One target directory per toolchain: cargo cannot see that the libraries
/// under an archive changed, and a toolchain directory is named by its key.
fn build_archive(root: &Path, toolchain: &Path, target: &str) -> PathBuf {
    let key = toolchain.file_name().expect("a toolchain directory").to_string_lossy();
    let target_dir = root.join(LIBC).join("target").join(format!("sysroot-{key}"));
    let mut cargo = Command::new("cargo");
    // A build run under `cargo test` or `cargo run` inherits that cargo's view
    // of which crate it is building; this one is libc's.
    for (var, _) in std::env::vars() {
        if var.starts_with("CARGO") || var == "RUSTC" || var == "RUSTFLAGS" {
            cargo.env_remove(&var);
        }
    }
    let output = cargo
        .env("RUSTUP_TOOLCHAIN", toolchain)
        .args(["rustc", "--release", "--target", target, "--crate-type", "staticlib"])
        .arg("--manifest-path")
        .arg(root.join(LIBC).join("Cargo.toml"))
        .arg("--target-dir")
        .arg(&target_dir)
        .output()
        .unwrap_or_else(|e| panic!("run cargo for libc's staticlib: {e}"));
    assert!(
        output.status.success(),
        "libc's staticlib for {target} did not build:\n{}",
        String::from_utf8_lossy(&output.stderr),
    );
    let archive = target_dir.join(format!("{target}/release/libtoyos_libc.a"));
    assert!(archive.is_file(), "cargo reported success and left no {}", archive.display());
    archive
}

/// Every file under `dir`, as its path below `dir` and its bytes, sorted.
fn files(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(at) = stack.pop() {
        for entry in fs::read_dir(&at).unwrap_or_else(|e| panic!("read {}: {e}", at.display())).flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let bytes = fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
                out.push((path.strip_prefix(dir).expect("under dir").to_path_buf(), bytes));
            }
        }
    }
    out.sort();
    out
}

/// Lay `headers` and `archive` out as a C sysroot for `target` under `root`'s
/// `target/toyos-c/`, at the name their content hashes to, and return it.
fn place(root: &Path, headers: &Path, archive: &Path, target: &str) -> PathBuf {
    let headers = files(headers);
    let archive = fs::read(archive).unwrap_or_else(|e| panic!("read {}: {e}", archive.display()));
    let mut hash = Sha256::new();
    hash.update(target.as_bytes());
    for (path, bytes) in &headers {
        hash.update(path.to_string_lossy().as_bytes());
        hash.update([0]);
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    }
    hash.update(&archive);
    let name: String = hash.finalize().iter().take(8).map(|b| format!("{b:02x}")).collect();
    let dir = root.join("target/toyos-c").join(name).join(target);
    if dir.is_dir() {
        return dir;
    }

    let partial = dir.with_extension(format!("partial-{}", std::process::id()));
    let _ = fs::remove_dir_all(&partial);
    for (path, bytes) in &headers {
        let to = partial.join("include").join(path);
        fs::create_dir_all(to.parent().expect("a header has a directory"))
            .unwrap_or_else(|e| panic!("create {}: {e}", to.display()));
        fs::write(&to, bytes).unwrap_or_else(|e| panic!("write {}: {e}", to.display()));
    }
    let lib = partial.join("lib");
    fs::create_dir_all(&lib).unwrap_or_else(|e| panic!("create {}: {e}", lib.display()));
    fs::write(lib.join("libtoyos_c.a"), &archive).unwrap_or_else(|e| panic!("write {}: {e}", lib.display()));
    // Another process may have placed the same content first; its copy is ours.
    if fs::rename(&partial, &dir).is_err() {
        let _ = fs::remove_dir_all(&partial);
        assert!(dir.is_dir(), "{} could not be placed, and nobody else placed it", dir.display());
    }
    dir
}

#[cfg(test)]
mod tests {
    use super::*;
    use toyos_tmpdir::TempDir;

    fn write(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    /// **A C sysroot is named by its content**: the same headers and archive
    /// are the same directory, found rather than rewritten; a header's edit or
    /// another archive is another; and what is placed is what clang reads.
    #[test]
    fn a_c_sysroot_is_named_by_its_headers_and_archive() {
        let base = TempDir::new("csysroot");
        let headers = base.join("include");
        write(&headers.join("stdio.h"), b"int puts(const char *);\n");
        write(&headers.join("sys/types.h"), b"typedef long ssize_t;\n");
        let archive = base.join("libtoyos_libc.a");
        write(&archive, b"!<arch>\n");

        let first = place(&base, &headers, &archive, "x86_64-unknown-toyos");
        assert!(first.ends_with("x86_64-unknown-toyos"));
        assert_eq!(fs::read(first.join("include/sys/types.h")).unwrap(), b"typedef long ssize_t;\n");
        assert_eq!(fs::read(first.join("lib/libtoyos_c.a")).unwrap(), b"!<arch>\n");

        let marker = first.join("include/stdio.h");
        let written = fs::metadata(&marker).unwrap().modified().unwrap();
        assert_eq!(place(&base, &headers, &archive, "x86_64-unknown-toyos"), first);
        assert_eq!(fs::metadata(&marker).unwrap().modified().unwrap(), written, "a placed sysroot was rewritten");

        write(&headers.join("stdio.h"), b"int puts(const char *s);\n");
        let edited = place(&base, &headers, &archive, "x86_64-unknown-toyos");
        assert_ne!(edited, first, "a header edit kept the old sysroot");
        write(&archive, b"!<arch>\nanother");
        assert_ne!(place(&base, &headers, &archive, "x86_64-unknown-toyos"), edited, "another archive kept the old sysroot");
        assert_ne!(
            place(&base, &headers, &archive, "aarch64-unknown-toyos").parent(),
            place(&base, &headers, &archive, "x86_64-unknown-toyos").parent(),
            "two targets' sysroots share one name"
        );
        let leftovers: Vec<_> = fs::read_dir(base.join("target/toyos-c"))
            .unwrap()
            .flatten()
            .flat_map(|d| fs::read_dir(d.path()).unwrap().flatten())
            .filter(|e| e.file_name().to_string_lossy().contains("partial"))
            .collect();
        assert!(leftovers.is_empty(), "a partial directory was left behind: {leftovers:?}");
    }
}
