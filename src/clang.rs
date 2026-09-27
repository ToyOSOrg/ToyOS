//! The C compiler a toolchain directory carries: the clang of the LLVM its
//! rustc links, so one build of one fork compiles ToyOS's Rust and its C.
//!
//! **Beside `rust-lld`, in `lib/rustlib/<host>/bin/`**, which every copy of a
//! toolchain directory — a compiler of a worktree's own, a sysroot, the
//! published release — carries whole. Bootstrap puts LLVM's tools there
//! already, `llvm-ar` — the archiver `cc` builds a C library with, where the
//! host's may not index ELF at all — and `llvm-readobj`, the ELF reader the
//! harness judges a linked C program by, among them. This adds the rest:
//!
//! - `clang`, the driver whose ToyOS toolchain (`src/llvm-project`'s
//!   `clang/lib/Driver/ToolChains/ToyOS.cpp`) names `ld.lld`, the sysroot and
//!   `-ltoyos_c`;
//! - `ld.lld`, a link to `rust-lld` — the same LLD, which takes its flavour from
//!   the name it is run by — found by clang in its own directory;
//! - `../lib/clang/<version>/include`, clang's own headers (`stddef.h`,
//!   `stdarg.h`), which it looks for relative to itself.
//!
//! Bootstrap removes `stage2` on every assemble, so these are put back after
//! every build that makes one, and a toolchain directory without all of it is
//! refused rather than left to fail at the first C compile.

use std::fs;
use std::path::{Path, PathBuf};

use crate::sysroot::clone_tree;
use crate::toolchain::host_triple;

/// What a toolchain directory's `bin` must hold for C: bootstrap's two, then
/// the two [`provision`] adds.
const TOOLS: [&str; 4] = ["llvm-ar", "llvm-readobj", "clang", "ld.lld"];

/// `lib/rustlib/<host>/bin` of `toolchain`, where `rust-lld` is.
fn bin(toolchain: &Path) -> PathBuf {
    toolchain.join("lib/rustlib").join(host_triple()).join("bin")
}

/// The clang `toolchain` carries.
pub fn clang(toolchain: &Path) -> PathBuf {
    bin(toolchain).join("clang")
}

/// The `llvm-ar` `toolchain` carries.
pub fn ar(toolchain: &Path) -> PathBuf {
    bin(toolchain).join("llvm-ar")
}

/// The `llvm-readobj` `toolchain` carries.
pub fn readobj(toolchain: &Path) -> PathBuf {
    bin(toolchain).join("llvm-readobj")
}

/// clang's resource directory's parent: `lib/clang`, beside `bin`.
fn resource_parent(toolchain: &Path) -> PathBuf {
    toolchain.join("lib/rustlib").join(host_triple()).join("lib/clang")
}

/// What of the C toolchain `toolchain` lacks, by name.
fn absent(toolchain: &Path) -> Vec<String> {
    let bin = bin(toolchain);
    let mut gone: Vec<String> = TOOLS
        .iter()
        .map(|name| bin.join(name))
        .filter(|path| !path.exists())
        .map(|path| path.display().to_string())
        .collect();
    let headers = fs::read_dir(resource_parent(toolchain))
        .ok()
        .and_then(|mut d| d.next())
        .and_then(Result::ok)
        .map(|version| version.path().join("include/stddef.h"));
    match headers {
        Some(stddef) if stddef.is_file() => {}
        _ => gone.push(format!("{}/<version>/include", resource_parent(toolchain).display())),
    }
    gone
}

/// Whether `toolchain` lacks any of the C toolchain.
pub(crate) fn missing(toolchain: &Path) -> bool {
    !absent(toolchain).is_empty()
}

/// Refuse a toolchain directory without its C toolchain.
pub(crate) fn assert_present(toolchain: &Path) {
    let gone = absent(toolchain);
    assert!(
        gone.is_empty(),
        "the toyos toolchain at {} carries no {}, and every C compile needs it: \
         `clang::provision` puts it there after each build that makes a stage2, and it did not",
        toolchain.display(),
        gone.join(", "),
    );
}

/// The one version directory under an LLVM build's `lib/clang`.
fn resource_version(llvm: &Path) -> PathBuf {
    let parent = llvm.join("lib/clang");
    let versions: Vec<PathBuf> = fs::read_dir(&parent)
        .unwrap_or_else(|e| panic!("read {}: {e} — was LLVM built with clang = true?", parent.display()))
        .flatten()
        .map(|e| e.path())
        .collect();
    match versions.as_slice() {
        [one] => one.clone(),
        other => panic!("{} holds {} clang versions, not one: {other:?}", parent.display(), other.len()),
    }
}

/// Give `toolchain` the C toolchain of the LLVM installed at `llvm`, replacing
/// whatever it carried.
pub(crate) fn provision(toolchain: &Path, llvm: &Path) {
    let bin = bin(toolchain);
    let from = llvm.join("bin/clang");
    let to = bin.join("clang");
    let _ = fs::remove_file(&to);
    // `fs::copy` follows `clang`'s link to `clang-<version>`, and clones where
    // the filesystem can.
    fs::copy(&from, &to).unwrap_or_else(|e| panic!("copy {} -> {}: {e}", from.display(), to.display()));
    let lld = bin.join("ld.lld");
    let _ = fs::remove_file(&lld);
    std::os::unix::fs::symlink("rust-lld", &lld)
        .unwrap_or_else(|e| panic!("symlink {} -> rust-lld: {e}", lld.display()));

    let version = resource_version(llvm);
    let into = resource_parent(toolchain).join(version.file_name().expect("a version directory"));
    let _ = fs::remove_dir_all(resource_parent(toolchain));
    clone_tree(&version.join("include"), &into.join("include"));
    assert_present(toolchain);
}

#[cfg(test)]
mod tests {
    use super::*;
    use toyos_tmpdir::TempDir;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    /// An LLVM install as bootstrap leaves one: `clang` a link to the
    /// versioned binary, the resource headers under `lib/clang/<version>`.
    fn llvm(base: &Path) -> PathBuf {
        let llvm = base.join("llvm");
        write(&llvm.join("bin/clang-22"), "the clang");
        std::os::unix::fs::symlink("clang-22", llvm.join("bin/clang")).unwrap();
        write(&llvm.join("lib/clang/22/include/stddef.h"), "typedef long ptrdiff_t;");
        llvm
    }

    /// **A toolchain without its C compiler is refused by name**, and one
    /// provisioned from an LLVM carries the binary behind `clang`'s link, an
    /// `ld.lld` that is `rust-lld`, and clang's headers where clang looks.
    #[test]
    fn a_toolchain_carries_the_clang_of_its_llvm_or_is_refused() {
        let base = TempDir::new("clang");
        let llvm = llvm(&base);
        // What bootstrap's own assemble leaves beside `rust-lld`.
        let stage2 = base.join("stage2");
        write(&bin(&stage2).join("rust-lld"), "lld");
        write(&bin(&stage2).join("llvm-readobj"), "the reader");
        write(&bin(&stage2).join("llvm-ar"), "the archiver");

        assert!(missing(&stage2));
        let refused = std::panic::catch_unwind(|| assert_present(&stage2)).expect_err("no clang, and not refused");
        let said = refused.downcast_ref::<String>().expect("a formatted refusal");
        assert!(said.contains("clang") && said.contains("ld.lld") && said.contains("include"), "{said}");

        provision(&stage2, &llvm);
        assert!(!missing(&stage2));
        assert_eq!(fs::read_to_string(clang(&stage2)).unwrap(), "the clang");
        assert!(!fs::symlink_metadata(clang(&stage2)).unwrap().file_type().is_symlink(), "clang is a copy, not the link");
        assert_eq!(fs::read_link(bin(&stage2).join("ld.lld")).unwrap(), Path::new("rust-lld"));
        assert_eq!(fs::read_to_string(bin(&stage2).join("ld.lld")).unwrap(), "lld");
        assert_eq!(fs::read_to_string(readobj(&stage2)).unwrap(), "the reader");
        assert_eq!(fs::read_to_string(ar(&stage2)).unwrap(), "the archiver");
        let stddef = resource_parent(&stage2).join("22/include/stddef.h");
        assert_eq!(fs::read_to_string(stddef).unwrap(), "typedef long ptrdiff_t;");

        // Provisioning again, from another LLVM, replaces what was there.
        fs::remove_dir_all(llvm.join("lib/clang/22")).unwrap();
        write(&llvm.join("lib/clang/23/include/stddef.h"), "v23");
        provision(&stage2, &llvm);
        assert!(!resource_parent(&stage2).join("22").exists(), "the old headers stayed beside the new");
        assert_eq!(fs::read_to_string(resource_parent(&stage2).join("23/include/stddef.h")).unwrap(), "v23");

        fs::remove_file(readobj(&stage2)).unwrap();
        assert!(missing(&stage2), "a missing llvm-readobj went unnoticed");
    }
}
