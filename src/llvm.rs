//! The LLVM every host compiler links, with its clang and LLD: a product of the
//! store (`src/store.rs`), one per key on this host.
//!
//! **Its key** ([`key`]) is the `src/llvm-project` commit the fork's gitlink
//! names, the fork's `src/bootstrap`, the bootstrap configuration below,
//! [`RECIPE`], and the tools the host builds it with ([`host_tools`]). Its
//! directory is bootstrap's install of that LLVM and its clang, with its LLD in
//! `bin/` beside `llvm-config`. Every compiler build names it as the host's
//! `llvm-config` with `llvm-has-rust-patches`, so bootstrap builds no LLVM and
//! takes LLD from beside it as `rust-lld`; `clang::provision` copies its clang.
//!
//! **Nothing of the environment it is asked from reaches it but
//! [`ENVIRONMENT`]**: the build and every tool its key asks run with the rest
//! cleared ([`clear`]), the configuration names the C and C++ compilers by path,
//! and every host library LLVM would otherwise find and link is turned off
//! ([`NO_HOST_LIBRARIES`]).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use crate::store::{self, Kind, Sources};
use crate::sysroot::clone_tree;
use crate::toolchain::{self, host_triple};

/// What changes how a key's sources become an LLVM and is none of the other
/// parts: the build's targets and what is kept of it. Moving it moves every key.
const RECIPE: &str = "bootstrap build of src/llvm-project/llvm and src/llvm-project/lld; the install's bin, \
                      include and lib, and lld in bin, read-only; 2";

/// What of the caller's environment the LLVM build, and every tool its key
/// asks, sees:
/// - `PATH` finds what runs the build: the Python behind `./x`, git, curl,
///   Ninja, and CMake, which the key names by path and version. The C and C++
///   compilers it finds, the configuration names by path.
/// - `TMPDIR` is where those tools write what they discard; the sandbox a build
///   runs in may allow no other place.
///
/// Everything else is dropped on purpose, the proxy, CA-bundle and Nix
/// variables a fetch may need among them: such a host's fetch fails loudly.
const ENVIRONMENT: [&str; 2] = ["PATH", "TMPDIR"];

/// Every host library the pinned LLVM and clang look for, and link when they
/// find one, turned off by the CMake option that asks for it
/// (`llvm/CMakeLists.txt`, `llvm/cmake/config-ix.cmake`, `clang/CMakeLists.txt`):
/// what a host has installed never decides what is built. Bootstrap's LLD step
/// takes none of these: LLD looks for no library unless asked, and links what this
/// LLVM chose.
const NO_HOST_LIBRARIES: [&str; 12] = [
    "LLVM_ENABLE_ZLIB",
    "LLVM_ENABLE_ZSTD",
    "LLVM_ENABLE_LIBXML2",
    "CLANG_ENABLE_LIBXML2",
    "LLVM_ENABLE_LIBEDIT",
    "LLVM_ENABLE_LIBPFM",
    "LLVM_ENABLE_FFI",
    "LLVM_ENABLE_CURL",
    "LLVM_ENABLE_HTTPLIB",
    "LLVM_ENABLE_ICU",
    "LLVM_ENABLE_ICONV",
    "LLVM_ENABLE_Z3_SOLVER",
];

/// What of bootstrap's install an LLVM keeps: `build/` beside them is CMake's
/// tree, which nothing reads once the install is made.
const KEPT: [&str; 3] = ["bin", "include", "lib"];

/// What a compiler build and `clang::provision` read of an LLVM.
const TOOLS: [&str; 4] = ["bin/llvm-config", "bin/lld", "bin/clang", "bin/llvm-ar"];


/// The fork's LLVM checkout, and the tree of [`Sources`] naming its commit.
pub(crate) const LLVM: &str = "src/llvm-project";

/// The build directory the key's configuration names.
const KEYED_BUILD_DIR: &str = "<build-dir>";

/// An LLVM, held in use for as long as this lives.
pub struct Llvm {
    /// Its install: `bin/`, `include/`, `lib/`.
    pub dir: PathBuf,
    _held: store::Held,
}

/// The `[target.<host>]` lines that make a `bootstrap.toml` link the LLVM at
/// `dir` and take its LLD.
pub fn host_lines(dir: &Path) -> String {
    format!("llvm-config = \"{}\"\nllvm-has-rust-patches = true", dir.join("bin/llvm-config").display())
}

/// The key of the LLVM `sources` name.
pub fn key(sources: &Sources) -> String {
    let tools = host_tools();
    key_of(sources, RECIPE, &config_text(Path::new(KEYED_BUILD_DIR), &host_triple(), tools), &tools.identity)
}

fn key_of(sources: &Sources, recipe: &str, config: &str, tools: &str) -> String {
    store::key(recipe, &[config, sources.get(LLVM), sources.get("src/bootstrap"), tools])
}

/// Give `command` nothing of this process's environment but [`ENVIRONMENT`].
fn clear(command: &mut Command) {
    command.env_clear();
    for name in ENVIRONMENT {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
}

/// The tools this host builds an LLVM with, as the cleared environment finds
/// them.
struct HostTools {
    cc: PathBuf,
    cxx: PathBuf,
    /// The C and C++ compilers and CMake, each by its resolved path and all its
    /// `--version` says, and on macOS the SDK path and version `xcrun` resolves.
    identity: String,
}

fn host_tools() -> &'static HostTools {
    static TOOLS: OnceLock<HostTools> = OnceLock::new();
    TOOLS.get_or_init(|| tools_with(|question| asked(Command::new("xcrun").arg(question))))
}

/// [`host_tools`], with `xcrun`'s answer to each question it is asked, so a
/// test can stand in for it.
fn tools_with(xcrun: impl Fn(&str) -> String) -> HostTools {
    let [cc, cxx, cmake] = ["cc", "c++", "cmake"].map(on_path);
    let mut identity = String::new();
    for tool in [&cc, &cxx, &cmake] {
        let mut version = Command::new(tool);
        identity += &format!("{}\n{}", tool.display(), asked(version.arg("--version")));
    }
    if host_triple().ends_with("apple-darwin") {
        for question in ["--show-sdk-path", "--show-sdk-version"] {
            identity += &xcrun(question);
        }
    }
    HostTools { cc, cxx, identity }
}

/// Everything `command` prints, run with the environment [`clear`]ed; a
/// failure is refused.
fn asked(command: &mut Command) -> String {
    clear(command);
    let out = command.output().unwrap_or_else(|e| panic!("run {command:?}: {e}"));
    assert!(out.status.success(), "{command:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    format!("{}{}\n", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

/// `name` as the `PATH` of [`ENVIRONMENT`] finds it, resolved.
fn on_path(name: &str) -> PathBuf {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let found = std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| panic!("no `{name}` on PATH, and the LLVM build runs it"));
    fs::canonicalize(&found).unwrap_or_else(|e| panic!("resolve {}: {e}", found.display()))
}

/// The LLVM `sources` name, made from the fork checkout at `fork` if nobody on
/// this host has made it, and held in use for as long as the returned value
/// lives. `root` records its key.
pub fn resolve(root: &Path, rust_dir: &Path, fork: &Path, sources: &Sources) -> Llvm {
    choose(root, rust_dir, fork, sources, build_in_fork)
}

/// [`resolve`] with the build that makes an LLVM passed in, so a test can stand
/// in for bootstrap: `build` builds in the fork checkout it is given and returns
/// the build directory, holding `<host>/llvm` and `<host>/lld`.
fn choose(root: &Path, rust_dir: &Path, fork: &Path, sources: &Sources, build: impl Fn(&Path) -> PathBuf) -> Llvm {
    let key = key(sources);
    let held = store::get(root, rust_dir, Kind::Llvm, &key, |partial| fill(root, fork, &key, partial, &build));
    Llvm { dir: held.dir.clone(), _held: held }
}

/// Why `dir` is not a whole LLVM, if it is not.
fn defect(dir: &Path) -> Option<String> {
    let kept = KEPT.iter().map(|k| dir.join(k)).filter(|p| !p.is_dir());
    let tools = TOOLS.iter().map(|t| dir.join(t)).filter(|p| !p.is_file());
    let gone: Vec<String> = kept.chain(tools).map(|p| p.display().to_string()).collect();
    (!gone.is_empty()).then(|| format!("{} carries no {}", dir.display(), gone.join(", ")))
}

/// Build the LLVM `key` names from `fork` into `partial`.
fn fill(root: &Path, fork: &Path, key: &str, partial: &Path, build: &impl Fn(&Path) -> PathBuf) {
    eprintln!("Building LLVM {key} in {}: nobody on this host has", fork.display());
    let built = build(fork);
    let host = host_triple();
    for part in KEPT {
        clone_tree(&built.join(&host).join("llvm").join(part), &partial.join(part));
    }
    let lld = built.join(&host).join("lld/bin/lld");
    fs::copy(&lld, partial.join("bin/lld"))
        .unwrap_or_else(|e| panic!("copy {} -> {}: {e}", lld.display(), partial.join("bin/lld").display()));
    let again = self::key(&Sources::of(root, fork));
    assert!(
        again == key,
        "the fork's LLVM sources moved while LLVM {key} was being built (they now name {again}); \
         nothing was kept, and the next build makes the one they name"
    );
    store::assert_built_at_gitlinks(fork, &[LLVM], &format!("LLVM {key}"));
    if let Some(defect) = defect(partial) {
        panic!("LLVM {key} was made, and is not whole: {defect}");
    }
    fs::remove_dir_all(&built).unwrap_or_else(|e| panic!("remove {}: {e}", built.display()));
}

/// Bootstrap's build of LLVM, clang and LLD in `fork`, into its own build
/// directory, which it returns.
fn build_in_fork(fork: &Path) -> PathBuf {
    let host = host_triple();
    let build_dir = fork.join("build/toyos-llvm");
    fs::create_dir_all(&build_dir).unwrap_or_else(|e| panic!("create {}: {e}", build_dir.display()));
    let config = build_dir.join("bootstrap.toml");
    fs::write(&config, config_text(&build_dir, &host, host_tools()))
        .unwrap_or_else(|e| panic!("write {}: {e}", config.display()));
    let config = config.to_str().unwrap_or_else(|| panic!("{} is not UTF-8", config.display()));
    let args = ["build", "--config", config, "src/llvm-project/llvm", "src/llvm-project/lld"];
    let (ok, _) = toolchain::x_build_with(fork, &args, "LLVM", clear);
    assert!(ok, "the LLVM build in {} failed: its output above says why", fork.display());
    build_dir
}

/// Bootstrap's configuration for an LLVM: the options every compiler build's
/// `[llvm]` names, no host library, and `tools`' compilers, for the host alone.
fn config_text(build_dir: &Path, host: &str, tools: &HostTools) -> String {
    let off: Vec<String> = NO_HOST_LIBRARIES.iter().map(|option| format!("{option} = \"OFF\"")).collect();
    format!(
        r#"change-id = "ignore"
profile = "compiler"

[build]
build-dir = "{build_dir}"
host = ["{host}"]
target = ["{host}"]

[llvm]
{llvm}
build-config = {{ {off} }}

[target.{host}]
cc = "{cc}"
cxx = "{cxx}"
"#,
        build_dir = build_dir.display(),
        llvm = crate::clang::LLVM_CONFIG,
        off = off.join(", "),
        cc = tools.cc.display(),
        cxx = tools.cxx.display(),
    )
}


#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::store::tests::{estate, git, refusal, write, Estate, LLVM_B};

    /// Bootstrap's stand-in: what its LLVM and LLD builds leave in the build
    /// directory, CMake's tree among them.
    fn fake_build(fork: &Path) -> PathBuf {
        let built = fork.join("build/toyos-llvm");
        let _ = fs::remove_dir_all(&built);
        let install = built.join(host_triple()).join("llvm");
        for tool in ["llvm-config", "clang-22", "llvm-ar"] {
            write(&install.join("bin").join(tool), &format!("the {tool}"));
        }
        std::os::unix::fs::symlink("clang-22", install.join("bin/clang")).unwrap();
        write(&install.join("include/llvm/Config/llvm-config.h"), "#define LLVM_VERSION_MAJOR 22");
        write(&install.join("lib/libLLVMCore.a"), "core");
        write(&install.join("lib/clang/22/include/stddef.h"), "typedef long ptrdiff_t;");
        write(&install.join("build/CMakeCache.txt"), "the build tree");
        write(&built.join(host_triple()).join("lld/bin/lld"), "the lld");
        built
    }

    /// Check `fork`'s LLVM out at the commit `content` names, one commit in every
    /// checkout whatever came before it: what bootstrap leaves once it has built
    /// one. Returns it.
    fn check_out_llvm(fork: &Path, content: &str) -> String {
        let checkout = fork.join(LLVM);
        if !checkout.join(".git").exists() {
            git(&checkout, &["init", "-q"]);
        }
        write(&checkout.join("llvm/CMakeLists.txt"), content);
        git(&checkout, &["add", "-A"]);
        let tree = git(&checkout, &["write-tree"]);
        let out = Command::new("git")
            .args(["-c", "user.email=t@t", "-c", "user.name=t", "commit-tree", &tree, "-m", content])
            .envs([("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z"), ("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z")])
            .current_dir(&checkout)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let commit = String::from_utf8(out.stdout).unwrap().trim().to_string();
        git(&checkout, &["reset", "-q", "--hard", &commit]);
        commit
    }

    /// [`check_out_llvm`], and that commit recorded as `fork`'s gitlink.
    fn pin_llvm(fork: &Path, content: &str) {
        let commit = check_out_llvm(fork, content);
        git(fork, &["update-index", "--add", "--cacheinfo", &format!("160000,{commit},{LLVM}")]);
        git(fork, &["commit", "-qm", &format!("LLVM {content}")]);
    }

    /// `estate`, every fork's LLVM checked out at the one commit its gitlink
    /// records.
    fn estate_built(name: &str) -> Estate {
        let e = estate(name);
        for worktree in [&e.same, &e.a, &e.b] {
            pin_llvm(&worktree.join("rust"), "A");
        }
        pin_llvm(&e.rust_dir, "A");
        e
    }

    fn sources(root: &Path) -> Sources {
        Sources::of(root, &root.join("rust"))
    }

    /// **One LLVM per key, made once and found again**: worktrees whose
    /// compilers differ and whose LLVM commit is one share one LLVM, and the
    /// later ones make nothing; what is kept is the install and its LLD, never
    /// CMake's tree, and the build directory goes.
    #[test]
    fn two_compilers_of_one_llvm_make_it_once() {
        let e = estate_built("llvm");
        let makes = Cell::new(0);
        let once = |fork: &Path| {
            makes.set(makes.get() + 1);
            assert_eq!(makes.get(), 1, "an LLVM whose key was made was made again");
            fake_build(fork)
        };
        let la = choose(&e.a, &e.rust_dir, &e.a.join("rust"), &sources(&e.a), once);
        assert_eq!(defect(&la.dir), None);
        assert_eq!(fs::read_to_string(la.dir.join("bin/lld")).unwrap(), "the lld");
        assert_eq!(fs::read_link(la.dir.join("bin/clang")).unwrap(), Path::new("clang-22"));
        assert!(!la.dir.join("build").exists(), "CMake's tree was kept");
        assert!(!e.a.join("rust/build/toyos-llvm").exists(), "the build directory outlived the placement");
        for root in [&e.b, &e.same, &e.primary] {
            assert_eq!(choose(root, &e.rust_dir, &root.join("rust"), &sources(root), once).dir, la.dir, "one LLVM commit named two LLVMs");
        }
    }

    /// **A placed LLVM cannot be written**, through its own path or through a
    /// link bootstrap makes to one of its files, and nothing in it can be
    /// removed, replaced or added.
    #[test]
    fn a_placed_llvm_is_never_written() {
        let e = estate_built("llvm-read-only");
        let dir = choose(&e.a, &e.rust_dir, &e.a.join("rust"), &sources(&e.a), fake_build).dir;
        let stage = e.a.join("stage1-rust-lld");
        fs::hard_link(dir.join("bin/lld"), &stage).unwrap();
        let denied = |what: &str, done: std::io::Result<()>| {
            assert_eq!(done.map_err(|e| e.kind()).err(), Some(std::io::ErrorKind::PermissionDenied), "{what}");
        };
        for file in [dir.join("bin/lld"), stage, dir.join("lib/libLLVMCore.a")] {
            denied(&format!("{} could be written", file.display()), fs::OpenOptions::new().write(true).open(&file).map(drop));
        }
        denied("a placed tool could be removed", fs::remove_file(dir.join("bin/lld")));
        denied("a file could be added to a placed LLVM", fs::write(dir.join("bin/new"), "x"));
    }

    /// **A make that leaves an LLVM not whole is refused, and nothing is
    /// placed.**
    #[test]
    fn an_llvm_made_not_whole_is_never_placed() {
        let e = estate_built("llvm-whole");
        let without_lld = |fork: &Path| {
            let built = fake_build(fork);
            fs::write(built.join(host_triple()).join("lld/bin/lld"), "").unwrap();
            fs::remove_file(built.join(host_triple()).join("llvm/bin/llvm-ar")).unwrap();
            built
        };
        let said = refusal("an LLVM without llvm-ar was placed", || {
            choose(&e.a, &e.rust_dir, &e.a.join("rust"), &sources(&e.a), without_lld);
        });
        assert!(said.contains("is not whole") && said.contains("bin/llvm-ar"), "{said}");
        assert!(!Kind::Llvm.dir(&e.rust_dir).join(key(&sources(&e.a))).exists());
    }

    /// **The key is the LLVM and nothing else**: each of the recipe, the
    /// configuration (its `[llvm]` and its host), the host's tools, the LLVM
    /// gitlink and `src/bootstrap` moves it; a compiler edit does not.
    #[test]
    fn the_key_moves_with_the_llvm_and_only_with_it() {
        let e = estate("llvm-key");
        let fork = e.same.join("rust");
        let tools = host_tools();
        let config = config_text(Path::new(KEYED_BUILD_DIR), &host_triple(), tools);
        let s = sources(&e.same);
        let base = key(&s);
        assert_eq!(key_of(&s, RECIPE, &config, &tools.identity), base);
        let linux = config_text(Path::new(KEYED_BUILD_DIR), "x86_64-unknown-linux-gnu", tools);
        for (what, other) in [
            ("the recipe", key_of(&s, "another recipe", &config, &tools.identity)),
            ("the [llvm]", key_of(&s, RECIPE, &config.replace("X86", "RISCV;X86"), &tools.identity)),
            ("the host", key_of(&s, RECIPE, &linux, &tools.identity)),
            ("the host's tools", key_of(&s, RECIPE, &config, "/usr/bin/gcc\ngcc 14\n")),
        ] {
            assert_ne!(other, base, "{what} did not move the key");
        }
        assert_eq!(key(&sources(&e.a)), base, "a compiler/ edit moved the LLVM key");

        git(&fork, &["update-index", "--add", "--cacheinfo", &format!("160000,{LLVM_B},{LLVM}")]);
        git(&fork, &["commit", "-qm", "another LLVM"]);
        let moved = key(&sources(&e.same));
        assert_ne!(moved, base, "another LLVM commit kept the key");
        write(&fork.join("src/bootstrap/src/core/build_steps/llvm.rs"), "cfg.define(\"LLVM_ENABLE_ZLIB\", \"OFF\");\n");
        assert_ne!(key(&sources(&e.same)), moved, "another LLVM step in bootstrap kept the key");
    }

    /// **The tools the key names are the host's**: the C and C++ compilers the
    /// configuration names by path, and CMake, each by the file it resolves to
    /// and what its `--version` says; on macOS, the SDK.
    #[test]
    fn the_key_names_the_host_s_tools() {
        let tools = host_tools();
        let lines: Vec<&str> = tools.identity.lines().collect();
        for tool in [&tools.cc, &tools.cxx] {
            assert!(tool.is_absolute() && tool.is_file(), "{} is no compiler", tool.display());
            let at = lines.iter().position(|l| Path::new(l) == tool.as_path()).unwrap_or_else(|| panic!("{}", tools.identity));
            assert!(lines[at + 1].contains("version"), "{} said no version: {}", tool.display(), tools.identity);
        }
        assert!(lines.iter().any(|l| l.starts_with("cmake version")), "{}", tools.identity);
        if host_triple().ends_with("apple-darwin") {
            assert!(lines.iter().any(|l| l.ends_with(".sdk") && Path::new(l).is_dir()), "no SDK: {}", tools.identity);
        }
        let config = config_text(Path::new(KEYED_BUILD_DIR), "h", tools);
        let named = format!("[target.h]\ncc = \"{}\"\ncxx = \"{}\"\n", tools.cc.display(), tools.cxx.display());
        assert!(config.contains(&named), "{config}");
    }

    /// **Two SDK versions are two LLVMs**, though both answer one SDK path, as
    /// the unversioned `MacOSX.sdk` a Command Line Tools update moves does.
    #[test]
    #[cfg(target_os = "macos")]
    fn two_sdk_versions_are_two_keys() {
        let e = estate("llvm-sdk");
        let s = sources(&e.same);
        let [older, newer] = ["26.0", "27.0"].map(|version| {
            let tools = tools_with(|question| match question {
                "--show-sdk-path" => "/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk\n".to_string(),
                "--show-sdk-version" => format!("{version}\n"),
                other => panic!("xcrun was asked {other}"),
            });
            key_of(&s, RECIPE, &config_text(Path::new(KEYED_BUILD_DIR), &host_triple(), &tools), &tools.identity)
        });
        assert_ne!(older, newer, "two SDK versions at one SDK path named one LLVM");
    }

    /// **Every host library LLVM looks for is turned off by name**, in the
    /// `build-config` bootstrap passes to CMake after its own options.
    #[test]
    fn the_llvm_config_turns_every_host_library_off() {
        let config = config_text(Path::new(KEYED_BUILD_DIR), "h", host_tools());
        let off = "build-config = { LLVM_ENABLE_ZLIB = \"OFF\", LLVM_ENABLE_ZSTD = \"OFF\", \
                   LLVM_ENABLE_LIBXML2 = \"OFF\", CLANG_ENABLE_LIBXML2 = \"OFF\", LLVM_ENABLE_LIBEDIT = \"OFF\", \
                   LLVM_ENABLE_LIBPFM = \"OFF\", LLVM_ENABLE_FFI = \"OFF\", LLVM_ENABLE_CURL = \"OFF\", \
                   LLVM_ENABLE_HTTPLIB = \"OFF\", LLVM_ENABLE_ICU = \"OFF\", LLVM_ENABLE_ICONV = \"OFF\", \
                   LLVM_ENABLE_Z3_SOLVER = \"OFF\" }\n";
        assert!(config.contains(&format!("\n{off}")), "{config}");
    }

    const ROOT: &str = "TOYOS_LLVM_TEST_ROOT";

    /// What a caller's environment may hold that would reach an LLVM build:
    /// flags, compilers, tools, and the SDK and deployment target.
    const AMBIENT: [(&str, &str); 11] = [
        ("CFLAGS", "-O0"),
        ("CXXFLAGS", "-O0"),
        ("LDFLAGS", "-Wl,-no-such-flag"),
        ("CC", "/no/such/cc"),
        ("CXX", "/no/such/c++"),
        ("AR", "/no/such/ar"),
        ("RANLIB", "/no/such/ranlib"),
        ("SDKROOT", "/no/such.sdk"),
        ("MACOSX_DEPLOYMENT_TARGET", "10.9"),
        ("CMAKE", "/no/such/cmake"),
        ("CMAKE_TOOLCHAIN_FILE", "/no/such/toolchain.cmake"),
    ];

    /// **The caller's environment reaches neither the LLVM build nor its key**:
    /// a process holding every [`AMBIENT`] name keys the LLVM as this one does,
    /// and the bootstrap it runs, a script that writes down its environment,
    /// sees nothing but `PATH`, `TMPDIR` and what a shell sets itself, named
    /// here and not read from [`ENVIRONMENT`].
    #[test]
    fn the_caller_s_environment_reaches_neither_the_build_nor_the_key() {
        use std::os::unix::fs::PermissionsExt;
        let e = estate("llvm-environment");
        let fork = e.same.join("rust");
        write(&fork.join("library/Cargo.lock"), "# lock\n");
        write(&fork.join("x"), "#!/bin/sh\nenv > build/toyos-llvm/environment\n");
        fs::set_permissions(fork.join("x"), fs::Permissions::from_mode(0o755)).unwrap();

        let out = crate::dirlock::tests::rerun("llvm::tests::keyed_and_built").env(ROOT, &e.same).envs(AMBIENT).output().unwrap();
        assert!(out.status.success(), "{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        let built = fork.join("build/toyos-llvm");
        assert_eq!(fs::read_to_string(built.join("key")).unwrap(), key(&sources(&e.same)), "the caller's environment moved the key");
        let seen = fs::read_to_string(built.join("environment")).unwrap();
        let allowed = ["PATH", "TMPDIR", "BOOTSTRAP_SKIP_TARGET_SANITY", "PWD", "OLDPWD", "SHLVL", "_"];
        for name in seen.lines().filter_map(|l| l.split_once('=')).map(|(name, _)| name) {
            assert!(allowed.contains(&name), "the build saw {name}: {seen}");
        }
        assert!(seen.lines().any(|l| l.starts_with("PATH=")), "the build saw no PATH: {seen}");
    }

    /// The process the environment test runs: the key of the worktree in
    /// [`ROOT`] and its fork's build, the key written beside what the build wrote.
    #[test]
    #[ignore = "the process the environment test runs; never runs on its own"]
    fn keyed_and_built() {
        let root = PathBuf::from(std::env::var(ROOT).unwrap_or_else(|_| panic!("keyed_and_built ran without {ROOT}; it is not a test")));
        let key = key(&sources(&root));
        let built = build_in_fork(&root.join("rust"));
        fs::write(built.join("key"), key).unwrap();
    }

    /// **Only an LLVM built from what its key names is placed**: not from an
    /// LLVM checkout holding what no commit does, refused before anything is
    /// built; not from a checkout other than the gitlink's; not when the
    /// sources moved while it was being built.
    #[test]
    fn what_the_key_does_not_name_is_never_placed() {
        let e = estate_built("llvm-dirt");
        let fork = e.a.join("rust");
        let never = |_: &Path| -> PathBuf { panic!("an LLVM was built from a checkout no commit holds") };

        write(&fork.join(LLVM).join("llvm/lib/IR/Core.cpp"), "int core_edited;\n");
        let said = refusal("an uncommitted LLVM edit was built", || {
            choose(&e.a, &e.rust_dir, &fork, &sources(&e.a), never);
        });
        assert!(said.contains("holds changes no commit does"), "{said}");
        fs::remove_dir_all(fork.join(LLVM).join("llvm/lib")).unwrap();

        let lagging = |fork: &Path| {
            check_out_llvm(fork, "A");
            fake_build(fork)
        };
        pin_llvm(&fork, "B");
        let said = refusal("an LLVM checkout other than the gitlink's was placed", || {
            choose(&e.a, &e.rust_dir, &fork, &sources(&e.a), lagging);
        });
        assert!(said.contains(&format!("{} is at", fork.join(LLVM).display())) && said.contains("its gitlink names"), "{said}");

        let step = fork.join("src/bootstrap/src/core/build_steps/llvm.rs");
        let moving = |fork: &Path| {
            write(&step, "cfg.define(\"LLVM_ENABLE_ZSTD\", \"OFF\");\n");
            fake_build(fork)
        };
        let said = refusal("an LLVM whose sources moved while it was built was placed", || {
            choose(&e.a, &e.rust_dir, &fork, &sources(&e.a), moving);
        });
        assert!(said.contains("moved while LLVM"), "{said}");
        let placed: Vec<_> = fs::read_dir(Kind::Llvm.dir(&e.rust_dir)).into_iter().flatten().flatten().map(|e| e.file_name()).collect();
        assert!(placed.is_empty(), "placed: {placed:?}");
    }
}
