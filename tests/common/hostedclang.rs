//! clang and LLD built for ToyOS (`build/hostedclang.rs`), installed by `pkg`
//! and run inside a guest: M2 of `issues/toyos-builds-itself.md`.
//!
//! The package and its `SHA256SUMS` go on the ROOT of
//! `tests/images/hostedclangcase.toml` as fixtures, beside `hello.c` and what
//! the host's clang and LLD, of the same LLVM commit, made of it on the same
//! command line. The job installs the package into `/apps` on DATA with
//! `/system/bin/pkg`, compiles `hello.c` with the installed clang, has its
//! driver link it, which starts LLD as a child, runs it, and compares each
//! product with the host's.

use std::fs;
use std::process::Command;
use std::time::Duration;

use super::{compile, qemu};
use qemu::{BootOptions, QemuInstance};

/// The job, and the guest binary it runs.
const JOB: &str = "hosted_clang_hello";

/// Where on ROOT the job's directory is, and so where the guest reads it.
const STAGED: &str = "share/hosted-clang";

/// What `hello.c` prints.
const HELLO: &str = "hello from a C program compiled and linked inside ToyOS";

/// DATA's size: the installed toolchain is about 300 MB, which the profile's
/// 128 MiB does not hold.
const DATA_BYTES: u64 = 1 << 30;

/// The host's half: the package, its clang and LLD on `hello.c`, and the
/// guest's verdict on the bytes its own made of the same file with the same
/// arguments.
pub fn hosted_clang_hello() -> Result<(), String> {
    let hosted = toyos_build::hostedclang::ensure(&compile::repo_root());
    let sources = compile::repo_root().join("tests/guest/c");
    let case = compile::repo_root().join("tests/images/hostedclangcase.toml");

    let scratch = super::lane::dir().join(JOB);
    fs::create_dir_all(&scratch).map_err(|e| format!("create {}: {e}", scratch.display()))?;
    let (object, linked) = (scratch.join("hello.o"), scratch.join("a.out"));
    for (args, to) in [(&["-c"][..], &object), (&[][..], &linked)] {
        let mut cc = Command::new(&hosted.c.clang);
        cc.args(hosted.c.args()).args(args).arg("hello.c").arg("-o").arg(to).current_dir(&sources);
        host_run(cc)?;
    }
    let packaged = hosted.package(&scratch.join("package"));

    let mut staged: Vec<(String, Vec<u8>)> = Vec::new();
    let mut stage = |rel: &str, from: &std::path::Path| -> Result<(), String> {
        let bytes = fs::read(from).map_err(|e| format!("read {}: {e}", from.display()))?;
        staged.push((format!("{STAGED}/{rel}"), bytes));
        Ok(())
    };
    let archive = packaged.archive.file_name().and_then(|n| n.to_str()).expect("a package file name").to_string();
    stage(&archive, &packaged.archive)?;
    stage("SHA256SUMS", &packaged.sums)?;
    stage("src/hello.c", &sources.join("hello.c"))?;
    for (name, product) in [("hello.o", &object), ("a.out", &linked)] {
        let bytes = fs::read(product).map_err(|e| format!("read {}: {e}", product.display()))?;
        let sha256 = toyos_build::sha256_hex(&bytes);
        eprintln!("  [{JOB}] the host's {name}: {} bytes, sha256 {sha256}", bytes.len());
        stage(&format!("host/{name}"), product)?;
    }
    eprintln!("  [{JOB}] the package: {archive}, {}", fs::read_to_string(&packaged.sums).unwrap_or_default().trim());
    fs::remove_dir_all(&scratch).map_err(|e| format!("remove {}: {e}", scratch.display()))?;

    let options = BootOptions { extra_root_files: staged, data_bytes: Some(DATA_BYTES), ..Default::default() };
    let bin = qemu::build_toyos_bin(qemu::SUITE_ARCH, &compile::repo_root().join("tests/guest/rust-tests"), JOB);
    let mut qemu = QemuInstance::boot_with_options(&case, &[], &[(JOB.to_string(), bin)], options);
    let result = qemu.run_test(&format!("test_rs_{JOB} /system/{STAGED} {archive}"), Duration::from_secs(900));
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
    for line in result.stdout.lines().filter(|l| l.contains(&format!("{JOB}: ")) || l.contains("pkg: ")) {
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
