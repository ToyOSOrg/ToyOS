use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::{env, fs};

/// Root of the repository.
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

/// Directory containing the TinyCC test cases.
pub fn testcases_dir() -> PathBuf {
    repo_root().join("tests/testcases/tinycc")
}

/// Path to the toyos-libc crate.
fn libc_dir() -> PathBuf {
    repo_root().join("userland/libc")
}

/// The toyos libc archive every C test links against, and the linker that
/// links them: the `rust-lld` of the toolchain that built the archive.
///
/// Cargo decides whether the archive is stale, because it is the only thing
/// here that can: an existence check cannot see a source change, and an archive
/// that outlives the libc it was built from links the C tests against a libc
/// that is not in the tree. Once per process — 156 C tests link this.
///
/// A `staticlib`, not the sysroot's `libtoyos_c.a`: a C program has no Rust
/// crate for rustc to emit the allocator shim `alloc` calls through, and a
/// `staticlib` carries its own.
fn libc_archive_toyos() -> (PathBuf, PathBuf) {
    static ARCHIVE: OnceLock<(PathBuf, PathBuf)> = OnceLock::new();
    ARCHIVE
        .get_or_init(|| {
            let libc_dir = libc_dir();
            let target = "x86_64-unknown-toyos";

            let _slot = toyos_build::buildlock::build_slot(&repo_root(), "the libc archive");
            let mut lock = toyos_build::buildlock::shared(&repo_root(), "toyos-libc archive");
            let sysroot = toyos_build::toolchain::ensure(&repo_root(), false, &mut lock);
            // One target directory per sysroot: cargo cannot see that the std
            // under an archive changed, and a sysroot is named by its key.
            let key = sysroot.dir.file_name().expect("a sysroot directory").to_string_lossy();
            let target_dir = libc_dir.join("target").join(format!("sysroot-{key}"));
            let archive = target_dir.join(format!("{target}/release/libtoyos_libc.a"));

            let mut cmd = std::process::Command::new("cargo");
            for (var, _) in env::vars() {
                if var.starts_with("CARGO") || var == "RUSTC" || var == "RUSTFLAGS" {
                    cmd.env_remove(&var);
                }
            }
            let output = cmd
                .env("RUSTUP_TOOLCHAIN", &sysroot.dir)
                .args(["rustc", "--release", "--target", target, "--crate-type", "staticlib"])
                .arg("--manifest-path")
                .arg(libc_dir.join("Cargo.toml"))
                .arg("--target-dir")
                .arg(&target_dir)
                .output()
                .unwrap_or_else(|e| panic!("failed to run cargo for toyos-libc: {e}"));
            assert!(
                output.status.success(),
                "toyos-libc build failed:\n{}",
                String::from_utf8_lossy(&output.stderr),
            );

            assert!(archive.exists(), "expected staticlib at {}", archive.display());
            (archive, toyos_build::toolchain::rust_lld(&sysroot.dir))
        })
        .clone()
}

/// Include paths for toyos-libc headers.
fn toyos_include_paths() -> Vec<PathBuf> {
    vec![libc_dir().join("include")]
}

/// Compile a C test file to object bytes using toyos-cc for ToyOS.
/// Returns (main object bytes, companion object bytes).
pub fn compile_c(name: &str) -> (Vec<u8>, Vec<Vec<u8>>) {
    let dir = testcases_dir();
    let c_file = dir.join(format!("{name}.c"));
    let source = fs::read_to_string(&c_file)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", c_file.display()));

    let mut include_paths = toyos_include_paths();
    include_paths.push(dir.clone());

    let opts = toyos_cc::CompileOptions {
        include_paths,
        defines: Vec::new(),
        target: Some("x86_64-unknown-toyos".to_string()),
        opt_level: 0,
        force_includes: Vec::new(),
    };

    let obj = toyos_cc::compile(&source, &format!("{name}.c"), &opts);

    // Compile companion files (e.g., "104+_inline.c" for "104_inline")
    let mut extras = Vec::new();
    if let Some(idx) = name.find('_') {
        let prefix = &name[..idx];
        let file_suffix = &name[idx..];
        let companion_name = format!("{}+{}.c", prefix, file_suffix);
        let companion = dir.join(&companion_name);
        if companion.exists() {
            let companion_source = fs::read_to_string(&companion)
                .unwrap_or_else(|e| panic!("cannot read {}: {e}", companion.display()));
            let extra = toyos_cc::compile(&companion_source, &companion_name, &opts);
            extras.push(extra);
        }
    }

    (obj, extras)
}

/// Link object bytes as a PIE ELF for ToyOS. Returns the linked binary bytes.
///
/// The arguments are the ones rustc passes `rust-lld` for an
/// `x86_64-unknown-toyos` executable, with the entry and the C library named
/// because no Rust crate is there to name them.
pub fn link_toyos(obj: &[u8], extra_objs: &[Vec<u8>], name: &str) -> Vec<u8> {
    let (libc_path, rust_lld) = libc_archive_toyos();

    let obj_path = super::lane::dir().join(format!("{name}.o"));
    fs::write(&obj_path, obj).unwrap();
    let mut inputs: Vec<PathBuf> = vec![obj_path];
    for (i, extra) in extra_objs.iter().enumerate() {
        let p = super::lane::dir().join(format!("{name}-extra{i}.o"));
        fs::write(&p, extra).unwrap();
        inputs.push(p);
    }
    let out = super::lane::dir().join(format!("{name}.elf"));

    let output = std::process::Command::new(&rust_lld)
        .args(["-flavor", "gnu"])
        .args(&inputs)
        .arg(&libc_path)
        .args(["--eh-frame-hdr", "-z", "noexecstack", "--gc-sections", "-pie", "-O1"])
        .args(["-e", "_start", "-o"])
        .arg(&out)
        .output()
        .unwrap_or_else(|e| panic!("failed to run {}: {e}", rust_lld.display()));
    for p in &inputs {
        let _ = fs::remove_file(p);
    }
    // One line: the harness reads a failed link's first line for the symbol a
    // declared case stops on, and LLD reports each undefined symbol on its own.
    assert!(
        output.status.success(),
        "rust-lld could not link {name}: {}",
        String::from_utf8_lossy(&output.stderr).lines().collect::<Vec<_>>().join(" | "),
    );
    let linked = fs::read(&out).unwrap_or_else(|e| panic!("read {}: {e}", out.display()));
    let _ = fs::remove_file(&out);
    linked
}
