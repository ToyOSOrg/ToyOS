//! clang and LLD built for ToyOS (`src/hostedclang.rs`), run inside a guest:
//! M2a of `issues/toyos-builds-itself.md`.
//!
//! The binaries, clang's resource headers and the C sysroot's headers and
//! `libtoyos_c.a` go on the ROOT of `tests/hostedclangcase`, as fixtures
//! beside the image's own files: ROOT is memory the loader filled, so the
//! guest reads them with no disk and no server behind it. The job compiles and
//! links `hello.c` there, runs it, and compares each product with what the
//! host's clang and LLD, of the same LLVM commit, made of the same input on the
//! same command line.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use sha2::{Digest, Sha256};

use super::{compile, qemu};
use qemu::{BootOptions, QemuInstance};

/// The job, and the guest binary it runs.
const JOB: &str = "hosted_clang_hello";

/// Where on ROOT the job's directory is, and so where the guest reads it.
const STAGED: &str = "share/hosted-clang";

/// What `hello.c` prints.
const HELLO: &str = "hello from a C program compiled and linked inside ToyOS";

/// The host's half: its clang and LLD on `hello.c`, the guest's on the same
/// file with the same arguments, and the guest's verdict on their bytes.
pub fn hosted_clang_hello() -> Result<(), String> {
    let hosted = toyos_build::hostedclang::ensure(&compile::repo_root());
    let c = compile::c_sysroot();
    let case = compile::repo_root().join("tests/hostedclangcase");

    let scratch = super::lane::dir().join(JOB);
    fs::create_dir_all(&scratch).map_err(|e| format!("create {}: {e}", scratch.display()))?;
    let (object, linked) = (scratch.join("hello.o"), scratch.join("hello"));
    let mut cc = Command::new(&c.clang);
    cc.args(c.args()).args(["-c", "hello.c", "-o"]).arg(&object).current_dir(&case);
    host_run(cc)?;
    // The line the ToyOS driver hands LLD (`clang -###`), as the guest runs it.
    let lld = c.clang.with_file_name("ld.lld");
    let mut ld = Command::new(&lld);
    ld.arg(format!("--sysroot={}", c.dir.display()))
        .args(["-pie", "--eh-frame-hdr", "-o"])
        .arg(&linked)
        .arg(format!("-L{}", c.dir.join("lib").display()))
        .arg(&object)
        .arg("-ltoyos_c");
    host_run(ld)?;

    let mut staged: Vec<(String, Vec<u8>)> = Vec::new();
    let mut stage = |rel: &str, from: &Path| {
        let bytes = fs::read(from).unwrap_or_else(|e| panic!("read {}: {e}", from.display()));
        staged.push((format!("{STAGED}/{rel}"), bytes));
    };
    stage("bin/clang", &hosted.dir.join("bin/clang"));
    stage("bin/ld.lld", &hosted.dir.join("bin/ld.lld"));
    stage("sysroot/lib/libtoyos_c.a", &c.dir.join("lib/libtoyos_c.a"));
    stage("src/hello.c", &case.join("hello.c"));
    for (name, product) in [("hello.o", &object), ("hello", &linked)] {
        let bytes = fs::read(product).map_err(|e| format!("read {}: {e}", product.display()))?;
        eprintln!("  [{JOB}] the host's {name}: {} bytes, sha256 {:x}", bytes.len(), Sha256::digest(&bytes));
        stage(&format!("host/{name}"), product);
    }
    // clang finds its resource headers beside itself, `../lib/clang`.
    for (rel, from) in tree(&hosted.dir.join("lib/clang")) {
        stage(&format!("lib/clang/{rel}"), &from);
    }
    // The C headers alone: `include/c++` is libc++'s, which no C compile reads.
    for (rel, from) in tree(&c.dir.join("include")).into_iter().filter(|(rel, _)| !rel.starts_with("c++/")) {
        stage(&format!("sysroot/include/{rel}"), &from);
    }
    fs::remove_dir_all(&scratch).map_err(|e| format!("remove {}: {e}", scratch.display()))?;

    let options = BootOptions { extra_root_files: staged, ..Default::default() };
    let bin = qemu::build_toyos_bin(qemu::SUITE_ARCH, &compile::repo_root().join("tests/toyos-rust-tests"), JOB);
    let mut qemu = QemuInstance::boot_with_options(&case, &[], &[(JOB.to_string(), bin)], options);
    let result = qemu.run_test(&format!("test_rs_{JOB} /system/{STAGED}"), Duration::from_secs(600));
    if let Some(why) = &result.error {
        return Err(format!("{why}\nthe job said:\n{}", result.stdout));
    }
    if result.exit_code != Some(0) {
        return Err(format!("the job ended {:?}:\n{}", result.exit_code, result.stdout));
    }
    for said in [&format!("{JOB}: hello said: {HELLO}"), &format!("{JOB}: ok")] {
        if !result.stdout.lines().any(|l| l.trim_end().ends_with(said.as_str())) {
            return Err(format!("the job never said `{said}`:\n{}", result.stdout));
        }
    }
    for line in result.stdout.lines().filter(|l| l.contains(&format!("{JOB}: "))) {
        eprintln!("  [{JOB}] {}", line.trim());
    }
    Ok(())
}

/// Run a host tool, and refuse its failure with what it said.
fn host_run(mut command: Command) -> Result<(), String> {
    let done = command.output().map_err(|e| format!("run {command:?}: {e}"))?;
    if !done.status.success() {
        return Err(format!(
            "{command:?} ended {}:\n{}{}",
            done.status,
            String::from_utf8_lossy(&done.stdout),
            String::from_utf8_lossy(&done.stderr)
        ));
    }
    Ok(())
}

/// Every file under `dir`, by its path relative to `dir`.
fn tree(dir: &Path) -> Vec<(String, std::path::PathBuf)> {
    let mut files = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(at) = pending.pop() {
        for entry in fs::read_dir(&at).unwrap_or_else(|e| panic!("read {}: {e}", at.display())) {
            let path = entry.unwrap_or_else(|e| panic!("read {}: {e}", at.display())).path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let rel = path.strip_prefix(dir).expect("under the walked directory");
                files.push((rel.to_string_lossy().into_owned(), path));
            }
        }
    }
    files
}
