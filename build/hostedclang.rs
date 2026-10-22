//! clang and `ld.lld` for a ToyOS host: built for `x86_64-unknown-toyos` from
//! the LLVM commit the host's LLVM is (`build/llvm.rs`), by the toolchain's own
//! clang against its C sysroot (`clang::CSysroot`), and linked statically.
//! CMake builds them, beside bootstrap, until bootstrap's LLVM for a ToyOS
//! host makes both
//! (`issues/bootstrap-cannot-build-llvm-clang-and-lld-for-a-toyos-host.md`).
//!
//! **Made only when asked for**, by `cargo run -- --hosted-clang` and by the
//! guest test that runs it (`tests/common/hostedclang.rs`): its build is
//! LLVM's, and its key moves with every sysroot's, so no other build makes it.
//!
//! **A function of its key** ([`key_of`]): the host LLVM's key, which names the
//! commit, what of it the LLVM keeps, the revision and repository it says it
//! was built from, and the host's tools, n2 and CMake among them; the
//! sysroot's, which names the C library, the C++ runtime and the clang that
//! builds against them; and [`RECIPE`] with [`OPTIONS`]. Its sources are
//! [`SOURCES`] of the commit as that LLVM keeps them in the store
//! (`build/llvm.rs`), never a checkout's files. CMake builds LLVM's tablegens for
//! the build machine first, in a nested build of its own (`NATIVE`), with the C
//! and C++ compilers the LLVM key names.
//!
//! `hosted-clang/<key>/` in the store holds `bin/clang`, `bin/ld.lld` and
//! clang's resource headers in `lib/clang/<version>/include`, where clang looks
//! beside itself; once its [`SOURCE`] file exists it is read-only.
//!
//! **What a ToyOS machine installs is a package of it** ([`HostedClang::package`]):
//! `clang-v<version>-toyos-x86_64.tar.gz` beside the `SHA256SUMS` that verifies
//! it, which `/system/bin/pkg` installs as `/apps/clang`.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::arch::Arch;
use crate::buildlock::{Guard, Keyed};
use crate::clang::CSysroot;
use crate::keystore::{self, Key};
use crate::sysroot::clone_tree;

/// What changes how the key's sources become this product and is none of the
/// other parts. Moving it moves every key.
const RECIPE: &str = "CMake and n2 build of clang and lld from the SOURCES the host's LLVM keeps, for the TARGET \
                      against its C sysroot, saying the version, revision and repository the host's clang \
                      says; of the build, clang as bin/clang, lld as bin/ld.lld and clang's resource \
                      headers; 3";

/// The ToyOS the binaries run on.
const TARGET: Arch = Arch::X86_64;

/// What of the commit the build reads, as git's pathspecs the host's LLVM keeps:
/// LLVM, clang and LLD, the CMake modules they share, the third-party sources
/// LLVM compiles in, and libunwind's headers, which LLD's Mach-O port reads. No
/// test, which the configuration builds none of.
pub(crate) const SOURCES: [&str; 12] = [
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
    /// The C sysroot it was built against, which is the one it compiles
    /// against.
    pub c: CSysroot,
    /// The licence of the LLVM it was built from: clang's, LLD's, libc++'s
    /// and the resource headers'.
    licence: PathBuf,
    _using: Guard,
}

/// `cargo run -- --hosted-clang`: make this tree's, name each binary with its
/// size, and package it in `target/hosted-clang`.
pub fn dispatch(root: &Path) {
    let hosted = ensure(root);
    for (_, kept) in BINARIES {
        let binary = hosted.dir.join("bin").join(kept);
        let size = fs::metadata(&binary).unwrap_or_else(|e| panic!("stat {}: {e}", binary.display())).len();
        println!("{} ({size} bytes)", binary.display());
    }
    let packaged = hosted.package(&root.join("target/hosted-clang"));
    println!("{}, verified by {}", packaged.archive.display(), packaged.sums.display());
}

/// The package in its directory: the archive, and `SHA256SUMS` beside it.
pub struct Packaged {
    pub archive: PathBuf,
    pub sums: PathBuf,
}

/// The package's name: the directory it is under `/apps`, and the program in
/// its `bin` that a launch starts.
const PACKAGE: &str = "clang";

/// The links of the package's `bin`: the driver's other names, which it reads
/// its language from.
const LINKS: [(&str, &str); 2] = [("cc", "clang"), ("clang++", "clang")];

impl HostedClang {
    /// Write the package and its `SHA256SUMS` into `into`, and answer where.
    /// **A function of the product and the sysroot**: every entry in one
    /// order, with no owner or time, so one toolchain packs to one digest.
    ///
    /// Beside the binaries it holds clang's resource headers where clang looks
    /// for them, `lib/clang`, and the sysroot's `include` and `lib` at
    /// `sysroot`, which the configuration file clang reads beside itself names
    /// (`<triple>.cfg`, `clang/docs/UsersManual.rst`, "Configuration files"),
    /// so a compile there needs no `--sysroot`.
    pub fn package(&self, into: &Path) -> Packaged {
        pack(&self.dir, &self.c.dir, &self.licence, &said_version(&self.c.clang).0, into)
    }
}

/// [`HostedClang::package`] of the product at `product` and the C sysroot at
/// `sysroot`, LLVM `version` under the licence at `licence`.
fn pack(product: &Path, sysroot: &Path, licence: &Path, version: &str, into: &Path) -> Packaged {
    let name = format!("{PACKAGE}-v{version}-toyos-{}.tar.gz", TARGET.name());
    fs::create_dir_all(into).unwrap_or_else(|e| panic!("create {}: {e}", into.display()));
    let archive = into.join(&name);
    let file = fs::File::create(&archive).unwrap_or_else(|e| panic!("create {}: {e}", archive.display()));
    let gzip = flate2::write::GzEncoder::new(std::io::BufWriter::new(file), flate2::Compression::default());
    let mut tar = tar::Builder::new(gzip);
    let mut append = |path: &str, mode: u32, data: &[u8]| {
        let mut header = tar::Header::new_ustar();
        header.set_size(data.len() as u64);
        header.set_mode(mode);
        header.set_mtime(0);
        tar.append_data(&mut header, format!("{PACKAGE}/{path}"), data).unwrap_or_else(|e| panic!("pack {path}: {e}"));
    };
    append("LICENSE.TXT", 0o644, &read(licence));
    for (_, kept) in BINARIES {
        append(&format!("bin/{kept}"), 0o755, &read(&product.join("bin").join(kept)));
    }
    append(&format!("bin/{}.cfg", TARGET.userland()), 0o644, b"--sysroot=<CFGDIR>/../sysroot\n");
    for (rel, from) in files(&product.join("lib/clang")) {
        append(&format!("lib/clang/{rel}"), 0o644, &read(&from));
    }
    for part in ["include", "lib"] {
        for (rel, from) in files(&sysroot.join(part)) {
            append(&format!("sysroot/{part}/{rel}"), 0o644, &read(&from));
        }
    }
    for (link, target) in LINKS {
        let mut header = tar::Header::new_ustar();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        header.set_mtime(0);
        tar.append_link(&mut header, format!("{PACKAGE}/bin/{link}"), target)
            .unwrap_or_else(|e| panic!("pack bin/{link}: {e}"));
    }
    let packed = tar.into_inner().and_then(|gzip| gzip.finish()).and_then(|mut file| file.flush());
    packed.unwrap_or_else(|e| panic!("{}: {e}", archive.display()));

    let digest = crate::sha256_hex(&read(&archive));
    let sums = into.join("SHA256SUMS");
    fs::write(&sums, format!("{digest}  {name}\n")).unwrap_or_else(|e| panic!("write {}: {e}", sums.display()));
    Packaged { archive, sums }
}

fn read(path: &Path) -> Vec<u8> {
    fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Every file under `dir`, by its path relative to `dir`, in sorted order. A
/// link in a product is a defect of its maker, refused here rather than packed
/// as the file it points at.
fn files(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(at) = pending.pop() {
        for entry in fs::read_dir(&at).unwrap_or_else(|e| panic!("read {}: {e}", at.display())) {
            let path = entry.unwrap_or_else(|e| panic!("read {}: {e}", at.display())).path();
            let kind = fs::symlink_metadata(&path).unwrap_or_else(|e| panic!("stat {}: {e}", path.display())).file_type();
            assert!(!kind.is_symlink(), "{} is a link, which no product of the store holds", path.display());
            if kind.is_dir() {
                pending.push(path);
            } else {
                let rel = path.strip_prefix(dir).expect("under the walked directory");
                out.push((rel.to_string_lossy().into_owned(), path));
            }
        }
    }
    out.sort();
    out
}

/// This tree's, made if nobody on this host has made it.
pub fn ensure(root: &Path) -> HostedClang {
    let mut lock = crate::buildlock::shared(root, "the ToyOS-hosted clang");
    let sysroot = crate::toolchain::ensure(root, &mut lock);
    let fork = crate::sysroot::fork_checkout(root, &mut lock);
    let store = keystore::host();
    let llvm = crate::llvm::resolve_held(root, &store, &fork, &mut lock);
    resolve(&store, &fork, &llvm.dir, sysroot.dir(), &crate::n2::ninja(root))
}

/// The product of the LLVM at `llvm`, the one the fork `fork` names, and the
/// sysroot at `sysroot`, both held in use in `store`: made if nobody on this
/// host has made it, under `ninja`.
fn resolve(store: &Path, fork: &Path, llvm: &Path, sysroot: &Path, ninja: &Path) -> HostedClang {
    let key = key_of(&stored(llvm), &stored(sysroot), &options());
    let dir = Keyed::HostedClang.store(store).join(&key);
    let make = || place(fork, llvm, sysroot, ninja, &key, &dir);
    let using = keystore::made(store, Keyed::HostedClang, &key, || defect(&dir), make);
    HostedClang { dir, c: CSysroot::of(sysroot, TARGET), licence: llvm.join("src/llvm/LICENSE.TXT"), _using: using }
}

/// The key the store product at `dir` is filed under.
fn stored(dir: &Path) -> Key {
    let named = dir.file_name().and_then(|n| n.to_str()).and_then(Key::parse);
    named.unwrap_or_else(|| panic!("{} is no product of the store: its name is no key", dir.display()))
}

fn key_of(llvm: &Key, sysroot: &Key, options: &[(String, String)]) -> Key {
    let options: Vec<String> = options.iter().map(|(name, value)| format!("{name}={value}")).collect();
    let parts = [RECIPE, TARGET.userland(), &options.join("\n"), llvm.as_str(), sysroot.as_str()];
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
fn place(fork: &Path, llvm: &Path, sysroot: &Path, ninja: &Path, key: &Key, dir: &Path) {
    eprintln!("Building the ToyOS-hosted clang and LLD {key}: nobody on this host has");
    let scratch = dir.with_extension("build");
    keystore::remove(&scratch);
    let built = scratch.join("build");
    build(fork, &llvm.join("src"), &built, &CSysroot::of(sysroot, TARGET), ninja, &scratch);

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
    // The fork the version stamp was read from names the LLVM built, or this
    // is not the key's.
    let again = crate::llvm::key(fork);
    assert!(
        again == stored(llvm),
        "the fork moved while {key} was being built (it now names LLVM {again}); nothing was kept"
    );
    let source = partial.join(SOURCE);
    fs::write(&source, format!("{key}\n")).unwrap_or_else(|e| panic!("write {}: {e}", source.display()));
    crate::llvm::read_only(&partial);
    keystore::retire(dir);
    fs::rename(&partial, dir).unwrap_or_else(|e| panic!("rename {} -> {}: {e}", partial.display(), dir.display()));
    keystore::remove(&scratch);
}

/// Configure LLVM at `sources`, of the commit `fork` names, for `c`'s target,
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
    definitions.push(("LLVM_VERSION_SUFFIX".to_string(), said_version(&c.clang).1));
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

/// What the host's clang, the sysroot's, says its version is: LLVM's release,
/// which the package is named by, and what it says beyond it, bootstrap's
/// `LLVM_VERSION_SUFFIX`. Every object a clang compiles carries the whole in
/// its `.comment`, so the ToyOS-hosted one is built to say it too.
fn said_version(clang: &Path) -> (String, String) {
    let mut command = Command::new(clang);
    command.arg("--version");
    crate::llvm::clear(&mut command);
    let out = command.output().unwrap_or_else(|e| panic!("run {command:?}: {e}"));
    assert!(out.status.success(), "{command:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    let said = String::from_utf8_lossy(&out.stdout);
    let first = said.lines().next().unwrap_or_default();
    split_version(first).unwrap_or_else(|| panic!("{} says no version: {said}", clang.display()))
}

/// The version `line`, clang's first line of `--version`, names, split at its
/// first `-`: the suffix is empty without one.
fn split_version(line: &str) -> Option<(String, String)> {
    let version = line.strip_prefix("clang version ")?.split_whitespace().next()?;
    let at = version.find('-').unwrap_or(version.len());
    Some((version[..at].to_string(), version[at..].to_string()))
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
    fn the_version_splits_at_its_first_dash() {
        let rev = "(https://github.com/ToyOSOrg/llvm-project.git ceaf0fbb8440)";
        let split = split_version;
        assert_eq!(split(&format!("clang version 22.1.8-rust-dev {rev}")), Some(("22.1.8".into(), "-rust-dev".into())));
        assert_eq!(split(&format!("clang version 22.1.8 {rev}")), Some(("22.1.8".into(), String::new())));
        assert_eq!(split("Apple clang version 17.0.0 (clang-1700.0.13.5)"), None);
    }

    /// **The package is the product and the sysroot under one `clang`,
    /// POSIX ustar throughout**, with the driver's other names as links and
    /// the configuration that names the sysroot, and one input packs to one
    /// digest.
    #[test]
    fn the_package_is_the_product_and_the_sysroot_under_one_directory() {
        let scratch = TempDir::new("hosted-clang-package");
        let write = |rel: &str| {
            let path = scratch.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, rel).unwrap();
        };
        for rel in ["product/bin/clang", "product/bin/ld.lld", "product/lib/clang/22/include/stddef.h", "product/SOURCE"] {
            write(rel);
        }
        for rel in ["c/include/stdio.h", "c/include/c++/v1/vector", "c/lib/libtoyos_c.a", "c/toolchain.cmake", "LICENSE.TXT"] {
            write(rel);
        }
        let (product, c) = (scratch.join("product"), scratch.join("c"));
        let licence = scratch.join("LICENSE.TXT");
        let first = pack(&product, &c, &licence, "22.1.8", &scratch.join("one"));
        assert_eq!(first.archive.file_name().unwrap(), "clang-v22.1.8-toyos-x86_64.tar.gz");

        let gz = fs::File::open(&first.archive).unwrap();
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(gz));
        let mut got = Vec::new();
        for entry in tar.entries().unwrap() {
            let mut entry = entry.unwrap();
            assert_eq!(&entry.header().as_bytes()[257..265], b"ustar\x0000", "a header that is not POSIX ustar");
            let path = entry.path().unwrap().to_string_lossy().into_owned();
            let held = match entry.link_name().unwrap() {
                Some(target) => format!("-> {}", target.display()),
                None => {
                    let mut text = String::new();
                    std::io::Read::read_to_string(&mut entry, &mut text).unwrap();
                    text
                }
            };
            got.push((path, held));
        }
        let want = [
            ("clang/LICENSE.TXT", "LICENSE.TXT"),
            ("clang/bin/clang", "product/bin/clang"),
            ("clang/bin/ld.lld", "product/bin/ld.lld"),
            ("clang/bin/x86_64-unknown-toyos.cfg", "--sysroot=<CFGDIR>/../sysroot\n"),
            ("clang/lib/clang/22/include/stddef.h", "product/lib/clang/22/include/stddef.h"),
            ("clang/sysroot/include/c++/v1/vector", "c/include/c++/v1/vector"),
            ("clang/sysroot/include/stdio.h", "c/include/stdio.h"),
            ("clang/sysroot/lib/libtoyos_c.a", "c/lib/libtoyos_c.a"),
            ("clang/bin/cc", "-> clang"),
            ("clang/bin/clang++", "-> clang"),
        ];
        let want: Vec<(String, String)> = want.iter().map(|(p, h)| (p.to_string(), h.to_string())).collect();
        assert_eq!(got, want);

        let digest = |sums: &Path| fs::read_to_string(sums).unwrap();
        let again = pack(&product, &c, &licence, "22.1.8", &scratch.join("two"));
        assert_eq!(digest(&first.sums), digest(&again.sums), "one input packed to two digests");
        assert!(digest(&first.sums).ends_with("  clang-v22.1.8-toyos-x86_64.tar.gz\n"));
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
