//! clang and `ld.lld` for a ToyOS host: built for `x86_64-unknown-toyos` from
//! the LLVM commit the host's LLVM is (`src/llvm.rs`), by the toolchain's own
//! clang against its C sysroot (`clang::CSysroot`), and linked statically.
//! CMake builds them, beside bootstrap, until bootstrap's LLVM for a ToyOS
//! host makes both
//! (`issues/bootstrap-cannot-build-llvm-clang-and-lld-for-a-toyos-host.md`).
//!
//! **Made only when asked for** (`cargo run -- --hosted-clang`): its build is
//! LLVM's, and its key moves with every sysroot's, so no other build makes it.
//!
//! **A function of its key** ([`key`]): the host LLVM's key, which names the
//! commit, the revision and repository it says it was built from, and the
//! host's tools, n2 and CMake among them; the sysroot's, which names the C
//! library, the C++ runtime and the clang that builds against them; and
//! [`RECIPE`] with [`OPTIONS`]. Its sources are the commit's, written from the
//! fork's LLVM repository ([`export`]), never a checkout's files. CMake builds
//! LLVM's tablegens for the build machine first, in a nested build of its own
//! (`NATIVE`), with the C and C++ compilers the LLVM key names.
//!
//! `hosted-clang/<key>/` in the store holds `bin/clang`, `bin/ld.lld` and
//! clang's resource headers in `lib/clang/<version>/include`, where clang looks
//! beside itself; once its [`SOURCE`] file exists it is read-only.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::arch::Arch;
use crate::buildlock::{Guard, Keyed};
use crate::clang::CSysroot;
use crate::compiler::LLVM;
use crate::keystore::{self, Key};
use crate::sysroot::{clone_tree, git_out, gitlink};

/// What changes how the key's sources become this product and is none of the
/// other parts. Moving it moves every key.
const RECIPE: &str = "CMake and n2 build of clang and lld from the LLVM commit's SOURCES, for the TARGET \
                      against its C sysroot, saying the version, revision and repository the host's clang \
                      says; of the build, clang as bin/clang, lld as bin/ld.lld and clang's resource \
                      headers; 2";

/// The ToyOS the binaries run on.
const TARGET: Arch = Arch::X86_64;

/// What of the commit the build reads, as git's pathspecs: LLVM, clang and
/// LLD, the CMake modules they share, the third-party sources LLVM compiles in,
/// and libunwind's headers, which LLD's Mach-O port reads. No test, which the
/// configuration builds none of.
const SOURCES: [&str; 12] = [
    "llvm",
    "clang",
    "lld",
    "cmake",
    "third-party",
    "libunwind/include",
    ":(exclude)llvm/test",
    ":(exclude)llvm/unittests",
    ":(exclude)clang/test",
    ":(exclude)clang/unittests",
    ":(exclude)lld/test",
    ":(exclude)lld/unittests",
];

/// The CMake options beyond the compilers', the sysroot's and the host
/// libraries [`crate::llvm`] turns off, each with its reason.
const OPTIONS: [(&str, &str); 11] = [
    ("CMAKE_BUILD_TYPE", "Release"),
    ("LLVM_ENABLE_PROJECTS", "clang;lld"),
    // The two architectures ToyOS runs on, as the host's LLVM targets them
    // (`clang::LLVM_CONFIG`).
    ("LLVM_TARGETS_TO_BUILD", "AArch64;X86"),
    // What the binaries run on and compile for, where LLVM otherwise asks
    // `config.guess` about the build machine.
    ("LLVM_HOST_TRIPLE", TARGET.userland()),
    ("LLVM_DEFAULT_TARGET_TRIPLE", TARGET.userland()),
    ("LLVM_INCLUDE_TESTS", "OFF"),
    ("LLVM_INCLUDE_BENCHMARKS", "OFF"),
    ("LLVM_INCLUDE_EXAMPLES", "OFF"),
    ("LLVM_INCLUDE_DOCS", "OFF"),
    ("CLANG_INCLUDE_TESTS", "OFF"),
    ("CLANG_INCLUDE_DOCS", "OFF"),
];

/// The file a finished product carries last, naming its key.
const SOURCE: &str = "SOURCE";

/// The binaries it keeps, each as the build names it and as it is kept: lld
/// takes its flavour from the name it is run by.
const BINARIES: [(&str, &str); 2] = [("clang", "clang"), ("lld", "ld.lld")];

/// A ToyOS-hosted clang and LLD, held in use for as long as this lives.
pub struct HostedClang {
    pub dir: PathBuf,
    _using: Guard,
}

/// `cargo run -- --hosted-clang`: make this tree's, and name each binary with
/// its size.
pub fn dispatch(root: &Path) {
    let mut lock = crate::buildlock::shared(root, "the ToyOS-hosted clang");
    let sysroot = crate::toolchain::ensure(root, &mut lock);
    let fork = crate::sysroot::fork_checkout(root, &mut lock);
    let hosted = resolve(&keystore::host(), &fork, sysroot.dir(), &crate::n2::ninja(root));
    for (_, kept) in BINARIES {
        let binary = hosted.dir.join("bin").join(kept);
        let size = fs::metadata(&binary).unwrap_or_else(|e| panic!("stat {}: {e}", binary.display())).len();
        println!("{} ({size} bytes)", binary.display());
    }
}

/// The product the LLVM fork `fork` names and the sysroot `sysroot` holds, in
/// `store`: made if nobody on this host has made it, under `ninja`.
pub fn resolve(store: &Path, fork: &Path, sysroot: &Path, ninja: &Path) -> HostedClang {
    let key = key(fork, sysroot);
    let dir = Keyed::HostedClang.store(store).join(&key);
    let make = || place(fork, sysroot, ninja, &key, &dir);
    let using = keystore::made(store, Keyed::HostedClang, &key, || defect(&dir), make);
    HostedClang { dir, _using: using }
}

/// The key of what `fork`'s LLVM and the sysroot at `sysroot` make.
fn key(fork: &Path, sysroot: &Path) -> Key {
    let named = sysroot.file_name().and_then(|n| n.to_str()).and_then(Key::parse);
    let sysroot =
        named.unwrap_or_else(|| panic!("{} is no sysroot of the store: its name is no key", sysroot.display()));
    key_of(&crate::llvm::key(fork), &sysroot, &options())
}

fn key_of(llvm: &Key, sysroot: &Key, options: &[(String, String)]) -> Key {
    let options: Vec<String> = options.iter().map(|(name, value)| format!("{name}={value}")).collect();
    let parts = [RECIPE, TARGET.userland(), &SOURCES.join(" "), &options.join("\n"), llvm.as_str(), sysroot.as_str()];
    Key::of(parts.join("\n\0\n").as_bytes())
}

/// [`OPTIONS`], and every host library off.
fn options() -> Vec<(String, String)> {
    let off = crate::llvm::NO_HOST_LIBRARIES.iter().map(|name| (*name, "OFF"));
    OPTIONS.into_iter().chain(off).map(|(name, value)| (name.to_string(), value.to_string())).collect()
}

/// Why `dir` is not a finished product, if it is not.
fn defect(dir: &Path) -> Option<String> {
    if !dir.join(SOURCE).is_file() {
        return Some(format!("{} carries no {SOURCE}", dir.display()));
    }
    let binaries = BINARIES.iter().map(|(_, kept)| dir.join("bin").join(kept));
    let mut gone: Vec<String> = binaries.filter(|p| !p.is_file()).map(|p| p.display().to_string()).collect();
    let headers = fs::read_dir(dir.join("lib/clang")).ok().and_then(|mut d| d.next()).and_then(Result::ok);
    if !headers.is_some_and(|version| version.path().join("include/stddef.h").is_file()) {
        gone.push(dir.join("lib/clang/<version>/include").display().to_string());
    }
    (!gone.is_empty()).then(|| format!("{} carries no {}", dir.display(), gone.join(", ")))
}

/// Build what `key` names and put it at `dir`. The caller holds the key's lock.
fn place(fork: &Path, sysroot: &Path, ninja: &Path, key: &Key, dir: &Path) {
    eprintln!("Building the ToyOS-hosted clang and LLD {key}: nobody on this host has");
    let scratch = dir.with_extension("build");
    keystore::remove(&scratch);
    let sources = scratch.join("src");
    export(fork, &sources);
    let built = scratch.join("build");
    build(fork, &sources, &built, &CSysroot::of(sysroot, TARGET), ninja, &scratch);

    let partial = dir.with_extension("partial");
    keystore::remove(&partial);
    let bin = partial.join("bin");
    fs::create_dir_all(&bin).unwrap_or_else(|e| panic!("create {}: {e}", bin.display()));
    for (name, kept) in BINARIES {
        // `fs::copy` follows the link the build installs clang as.
        let (from, to) = (built.join("bin").join(name), bin.join(kept));
        fs::copy(&from, &to).unwrap_or_else(|e| panic!("copy {} -> {}: {e}", from.display(), to.display()));
    }
    let resource = crate::clang::resource_version(&built);
    let version = resource.file_name().unwrap_or_else(|| panic!("{} names no version", resource.display()));
    clone_tree(&resource.join("include"), &partial.join("lib/clang").join(version).join("include"));
    // What was built is what the key names, or it is not that key's.
    let again = self::key(fork, sysroot);
    assert!(again == *key, "the sources moved while {key} was being built (they now name {again}); nothing was kept");
    let source = partial.join(SOURCE);
    fs::write(&source, format!("{key}\n")).unwrap_or_else(|e| panic!("write {}: {e}", source.display()));
    crate::llvm::read_only(&partial);
    keystore::retire(dir);
    fs::rename(&partial, dir).unwrap_or_else(|e| panic!("rename {} -> {}: {e}", partial.display(), dir.display()));
    keystore::remove(&scratch);
}

/// Write [`SOURCES`] as the commit `fork`'s LLVM gitlink names holds them into
/// `dest`, from the LLVM repository of `fork`'s git directory: a linked
/// worktree's `rust/src/llvm-project` is no checkout.
fn export(fork: &Path, dest: &Path) {
    let common = git_out(fork, &["rev-parse", "--path-format=absolute", "--git-common-dir"]);
    let repository = Path::new(common.trim()).join("modules").join(LLVM);
    crate::llvm::check_out_committed(&repository, &gitlink(fork, LLVM), &SOURCES, dest);
}

/// Configure LLVM at `sources`, the commit `fork` names, for `c`'s target,
/// in `built`, and build clang and LLD there under `ninja`; `scratch` holds
/// the links CMake runs the compilers by.
fn build(fork: &Path, sources: &Path, built: &Path, c: &CSysroot, ninja: &Path, scratch: &Path) {
    // The driver links C++ when run as `clang++`, and LLVM's archiver indexes
    // as `ranlib` when run by that name.
    let links = scratch.join("bin");
    fs::create_dir_all(&links).unwrap_or_else(|e| panic!("create {}: {e}", links.display()));
    let (cxx, ranlib) = (links.join("clang++"), links.join("llvm-ranlib"));
    for (link, to) in [(&cxx, &c.clang), (&ranlib, &c.ar)] {
        std::os::unix::fs::symlink(to, link)
            .unwrap_or_else(|e| panic!("symlink {} -> {}: {e}", link.display(), to.display()));
    }
    let path = |p: &Path| p.display().to_string();
    let (cc, cxx_native) = crate::llvm::host_compilers();
    let mut native = vec![format!("-DCMAKE_C_COMPILER={}", path(&cc)), format!("-DCMAKE_CXX_COMPILER={}", path(&cxx_native))];
    native.extend(crate::llvm::NO_HOST_LIBRARIES.iter().map(|name| format!("-D{name}=OFF")));
    let mut definitions = options();
    definitions.extend(crate::llvm::stamp_options(fork));
    definitions.push(("LLVM_VERSION_SUFFIX".to_string(), version_suffix(&c.clang)));
    definitions.extend([
        ("CMAKE_TOOLCHAIN_FILE".to_string(), path(&c.cmake_toolchain())),
        ("CMAKE_C_COMPILER".to_string(), path(&c.clang)),
        ("CMAKE_ASM_COMPILER".to_string(), path(&c.clang)),
        ("CMAKE_CXX_COMPILER".to_string(), path(&cxx)),
        ("CMAKE_AR".to_string(), path(&c.ar)),
        ("CMAKE_RANLIB".to_string(), path(&ranlib)),
        ("CMAKE_MAKE_PROGRAM".to_string(), path(ninja)),
        ("CROSS_TOOLCHAIN_FLAGS_NATIVE".to_string(), native.join(";")),
    ]);
    let mut configure = Command::new("cmake");
    configure.args(["-G", "Ninja", "-Wno-dev", "-S"]).arg(sources.join("llvm")).arg("-B").arg(built);
    configure.args(definitions.iter().map(|(name, value)| format!("-D{name}={value}")));
    run(configure, "configuration", ninja);
    let mut make = Command::new(ninja);
    make.arg("-C").arg(built).args(BINARIES.map(|(name, _)| name));
    run(make, "build", ninja);
}

/// What the host's clang, the sysroot's, says it is beyond LLVM's version:
/// bootstrap's `LLVM_VERSION_SUFFIX`. Every object a clang compiles carries
/// what it says in its `.comment`, so this one says it too.
fn version_suffix(clang: &Path) -> String {
    let mut command = Command::new(clang);
    command.arg("--version");
    crate::llvm::clear(&mut command);
    let out = command.output().unwrap_or_else(|e| panic!("run {command:?}: {e}"));
    assert!(out.status.success(), "{command:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    let said = String::from_utf8_lossy(&out.stdout);
    let first = said.lines().next().unwrap_or_default();
    suffix(first).unwrap_or_else(|| panic!("{} says no version: {said}", clang.display()))
}

/// The suffix of the version `line`, clang's first line of `--version`, names:
/// what follows its first `-`, and none without one.
fn suffix(line: &str) -> Option<String> {
    let version = line.strip_prefix("clang version ")?.split_whitespace().next()?;
    Some(version.find('-').map_or(String::new(), |at| version[at..].to_string()))
}

/// Run `command` seeing nothing of this process's environment but what the
/// LLVM build does (`llvm::clear`), with `ninja`'s directory first on `PATH`
/// for the nested build CMake runs, and refuse its failure.
fn run(mut command: Command, what: &str, ninja: &Path) {
    crate::llvm::clear(&mut command);
    let dir = ninja.parent().unwrap_or_else(|| panic!("{} is in no directory", ninja.display()));
    let caller = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(std::iter::once(dir.to_path_buf()).chain(std::env::split_paths(&caller)))
        .unwrap_or_else(|e| panic!("{} cannot lead PATH: {e}", dir.display()));
    command.env("PATH", path);
    let status = command.status().unwrap_or_else(|e| panic!("run {command:?}: {e}"));
    assert!(status.success(), "the ToyOS-hosted clang's {what} failed ({status}): its output above says why");
}

#[cfg(test)]
mod tests {
    use toyos_tmpdir::TempDir;

    use super::*;

    fn key(text: &str) -> Key {
        Key::of(text.as_bytes())
    }

    /// **The key moves with each input and with nothing else**: the host
    /// LLVM's key, the sysroot's and every CMake option.
    #[test]
    fn the_key_moves_with_the_llvm_the_sysroot_and_the_options() {
        let (llvm, sysroot) = (key("llvm"), key("sysroot"));
        let base = key_of(&llvm, &sysroot, &options());
        assert_eq!(key_of(&llvm, &sysroot, &options()), base);
        let mut moved = options();
        moved[0].1 = "Debug".to_string();
        for (what, other) in [
            ("the LLVM", key_of(&key("another llvm"), &sysroot, &options())),
            ("the sysroot", key_of(&llvm, &key("another sysroot"), &options())),
            ("an option", key_of(&llvm, &sysroot, &moved)),
        ] {
            assert_ne!(other, base, "{what} did not move the key");
        }
    }

    /// **The suffix is what follows the version's first `-`**, none without
    /// one, and a line that names no clang version names none.
    #[test]
    fn the_suffix_is_what_follows_the_version() {
        let rev = "(https://github.com/ToyOSOrg/llvm-project.git ceaf0fbb8440)";
        assert_eq!(suffix(&format!("clang version 22.1.8-rust-dev {rev}")).as_deref(), Some("-rust-dev"));
        assert_eq!(suffix(&format!("clang version 22.1.8 {rev}")).as_deref(), Some(""));
        assert_eq!(suffix("Apple clang version 17.0.0 (clang-1700.0.13.5)"), None);
    }

    /// **A product missing a binary or clang's headers is not whole**, nor one
    /// without its `SOURCE`.
    #[test]
    fn a_product_without_a_binary_or_its_headers_is_not_whole() {
        let dir = TempDir::new("hosted-clang");
        let write = |rel: &str| {
            let path = dir.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, rel).unwrap();
        };
        assert!(defect(&dir).is_some_and(|d| d.contains(SOURCE)));
        for rel in [SOURCE, "bin/clang", "bin/ld.lld", "lib/clang/22/include/stddef.h"] {
            write(rel);
        }
        assert_eq!(defect(&dir), None);
        for gone in ["bin/ld.lld", "lib/clang/22/include/stddef.h"] {
            fs::remove_file(dir.join(gone)).unwrap();
            assert!(defect(&dir).is_some_and(|d| d.contains(gone.split("22").next().unwrap())), "{gone}: {:?}", defect(&dir));
            write(gone);
        }
    }
}
