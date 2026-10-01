//! The C compiler a toolchain directory carries: the clang of the LLVM its
//! rustc links, so one build of one fork compiles ToyOS's Rust and its C.
//!
//! **Beside `rust-lld`, in `lib/rustlib/<host>/bin/`**, which every copy of a
//! toolchain directory — a compiler of a worktree's own, a sysroot, the
//! published release — carries whole. Bootstrap copies none of LLVM's tools
//! there (`llvm-tools = false`); this puts the ones a build runs:
//!
//! - `clang`, the driver whose ToyOS toolchain (`src/llvm-project`'s
//!   `clang/lib/Driver/ToolChains/ToyOS.cpp`) names `ld.lld`, the sysroot and
//!   `-ltoyos_c`;
//! - `llvm-ar`, the archiver `cc` builds a C library with, where the host's may
//!   not index ELF at all;
//! - `ld.lld`, a link to `rust-lld` — the same LLD, which takes its flavour from
//!   the name it is run by — found by clang in its own directory;
//! - `../lib/clang/<version>/include`, clang's own headers (`stddef.h`,
//!   `stdarg.h`), which it looks for relative to itself;
//! - on an Apple host, `rust-objcopy`, LLVM's `llvm-objcopy`, which rustc runs
//!   from here to strip a Darwin binary.
//!
//! Bootstrap removes `stage2` on every assemble, so these are put back after
//! every build that makes one, and a toolchain directory without all of it is
//! refused rather than left to fail at the first C compile.

use std::fs;
use std::path::{Path, PathBuf};

use crate::arch::Arch;
use crate::sysroot::clone_tree;
use crate::toolchain::host_triple;

/// The LLVM every host compiler links, in every `bootstrap.toml` that builds
/// one: built from `src/llvm-project` — the fork that knows the ToyOS target —
/// with clang beside it, for the host and the two architectures ToyOS runs on.
pub(crate) const LLVM_CONFIG: &str = "download-ci-llvm = false\n\
                                      clang = true\n\
                                      targets = \"AArch64;X86\"\n\
                                      experimental-targets = \"\"";

/// What a toolchain directory's `bin` must hold for C.
const TOOLS: [&str; 3] = ["llvm-ar", "clang", "ld.lld"];

/// What an Apple host's toolchain directory's `bin` must hold besides.
const APPLE_STRIP: &str = "rust-objcopy";

/// [`TOOLS`], and on an Apple host [`APPLE_STRIP`].
fn tools() -> impl Iterator<Item = &'static str> {
    TOOLS.into_iter().chain(host_triple().ends_with("apple-darwin").then_some(APPLE_STRIP))
}

/// `lib/rustlib/<host>/bin` of `toolchain`, where `rust-lld` is.
fn bin(toolchain: &Path) -> PathBuf {
    toolchain.join("lib/rustlib").join(host_triple()).join("bin")
}

/// A C sysroot, and the clang that reads it.
#[derive(Clone, Debug)]
pub struct CSysroot {
    /// What `--sysroot` names: `include/` and `lib/`.
    pub dir: PathBuf,
    /// The clang of the toolchain the archive was built with.
    pub clang: PathBuf,
    /// That toolchain's archiver.
    pub ar: PathBuf,
    /// The guest target, as clang's `--target` spells it.
    pub target: &'static str,
}

impl CSysroot {
    /// `arch`'s, in the sysroot at `toolchain`: `lib/rustlib/<target>/c`.
    pub fn of(toolchain: &Path, arch: Arch) -> Self {
        let target = arch.userland();
        Self {
            dir: toolchain.join("lib/rustlib").join(target).join("c"),
            clang: bin(toolchain).join("clang"),
            ar: bin(toolchain).join("llvm-ar"),
            target,
        }
    }

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

/// clang's resource directory's parent: `lib/clang`, beside `bin`.
fn resource_parent(toolchain: &Path) -> PathBuf {
    toolchain.join("lib/rustlib").join(host_triple()).join("lib/clang")
}

/// What of the C toolchain `toolchain` lacks, by name.
fn absent(toolchain: &Path) -> Vec<String> {
    let bin = bin(toolchain);
    let mut gone: Vec<String> = tools()
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

/// Why `toolchain` cannot compile C, if it cannot.
pub(crate) fn defect(toolchain: &Path) -> Option<String> {
    let gone = absent(toolchain);
    (!gone.is_empty()).then(|| {
        format!(
            "the toyos toolchain at {} carries no {}, and every C compile needs it: \
             `clang::provision` puts it there after each build that makes a stage2, and it did not",
            toolchain.display(),
            gone.join(", "),
        )
    })
}

/// Refuse a toolchain directory without its C toolchain.
pub(crate) fn assert_present(toolchain: &Path) {
    if let Some(defect) = defect(toolchain) {
        panic!("{defect}");
    }
}

/// The one version directory under an LLVM build's `lib/clang`.
pub(crate) fn resource_version(llvm: &Path) -> PathBuf {
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

/// Give `stage2` the C toolchain of the LLVM at `llvm` (`src/llvm.rs`), the one
/// its rustc links.
pub(crate) fn provision(stage2: &Path, llvm: &Path) {
    let bin = bin(stage2);
    let apple = host_triple().ends_with("apple-darwin").then_some((crate::llvm::APPLE_TOOL, APPLE_STRIP));
    for (tool, name) in [("clang", "clang"), ("llvm-ar", "llvm-ar")].into_iter().chain(apple) {
        let (from, to) = (llvm.join("bin").join(tool), bin.join(name));
        let _ = fs::remove_file(&to);
        // `fs::copy` clones where the filesystem can.
        fs::copy(&from, &to).unwrap_or_else(|e| panic!("copy {} -> {}: {e}", from.display(), to.display()));
    }
    let lld = bin.join("ld.lld");
    let _ = fs::remove_file(&lld);
    std::os::unix::fs::symlink("rust-lld", &lld)
        .unwrap_or_else(|e| panic!("symlink {} -> rust-lld: {e}", lld.display()));

    let version = resource_version(llvm);
    let into = resource_parent(stage2).join(version.file_name().expect("a version directory"));
    let _ = fs::remove_dir_all(resource_parent(stage2));
    clone_tree(&version.join("include"), &into.join("include"));
    assert_present(stage2);
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
        write(&llvm.join("bin/llvm-ar"), "the archiver");
        write(&llvm.join("bin/llvm-objcopy"), "the objcopy");
        write(&llvm.join("lib/clang/22/include/stddef.h"), "typedef long ptrdiff_t;");
        llvm
    }

    /// **A toolchain without its C compiler is refused by name**, and one
    /// provisioned from an LLVM carries the binary behind `clang`'s link, that
    /// LLVM's archiver, an `ld.lld` that is `rust-lld`, clang's headers where
    /// clang looks, and on an Apple host the `rust-objcopy` rustc strips with.
    #[test]
    fn a_toolchain_carries_the_clang_of_its_llvm_or_is_refused() {
        let base = TempDir::new("clang");
        let llvm = llvm(&base);
        let apple = host_triple().ends_with("apple-darwin");
        // What bootstrap's own assemble leaves: `rust-lld` and no LLVM tool.
        let stage2 = base.join("stage2");
        write(&bin(&stage2).join("rust-lld"), "lld");

        assert!(defect(&stage2).is_some());
        let refused = std::panic::catch_unwind(|| assert_present(&stage2)).expect_err("no clang, and not refused");
        let said = refused.downcast_ref::<String>().expect("a formatted refusal");
        for named in ["clang", "llvm-ar", "ld.lld", "include"] {
            assert!(said.contains(named), "{named}: {said}");
        }
        assert_eq!(said.contains(APPLE_STRIP), apple, "{said}");

        provision(&stage2, &llvm);
        assert_eq!(defect(&stage2), None);
        assert_eq!(fs::read_to_string(bin(&stage2).join("clang")).unwrap(), "the clang");
        assert!(!fs::symlink_metadata(bin(&stage2).join("clang")).unwrap().file_type().is_symlink(), "clang is a copy, not the link");
        assert_eq!(fs::read_link(bin(&stage2).join("ld.lld")).unwrap(), Path::new("rust-lld"));
        assert_eq!(fs::read_to_string(bin(&stage2).join("ld.lld")).unwrap(), "lld");
        assert_eq!(fs::read_to_string(bin(&stage2).join("llvm-ar")).unwrap(), "the archiver");
        assert_eq!(fs::read_to_string(bin(&stage2).join(APPLE_STRIP)).ok().as_deref(), apple.then_some("the objcopy"));
        let stddef = resource_parent(&stage2).join("22/include/stddef.h");
        assert_eq!(fs::read_to_string(stddef).unwrap(), "typedef long ptrdiff_t;");

        // Provisioning again, from another LLVM, replaces what was there.
        fs::remove_dir_all(llvm.join("lib/clang/22")).unwrap();
        write(&llvm.join("lib/clang/23/include/stddef.h"), "v23");
        provision(&stage2, &llvm);
        assert!(!resource_parent(&stage2).join("22").exists(), "the old headers stayed beside the new");
        assert_eq!(fs::read_to_string(resource_parent(&stage2).join("23/include/stddef.h")).unwrap(), "v23");

        let lost = if apple { [APPLE_STRIP, "llvm-ar"].as_slice() } else { ["llvm-ar"].as_slice() };
        for tool in lost {
            provision(&stage2, &llvm);
            fs::remove_file(bin(&stage2).join(tool)).unwrap();
            assert!(defect(&stage2).is_some_and(|d| d.contains(tool)), "a missing {tool} went unnoticed");
        }
    }
}
