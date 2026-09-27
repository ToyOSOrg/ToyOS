//! A C program compiled and linked by the toolchain's clang — the ToyOS driver
//! `ToyOSOrg/llvm-project` carries — judged as the loader sees the file, and
//! then run on ToyOS.

use std::fs;
use std::process::Command;
use std::time::Duration;

use super::compile;
use super::qemu::{BootOptions, QemuInstance};

/// The program, beside the corpus it is not part of.
const HELLO: &str = "tests/testcases/hello.c";
/// What it prints, all of which its own arithmetic and libc produce.
const SAYS: &str = "hello from clang, on ToyOS: 6 * 7 = 42";

/// What an image the ToyOS driver links must be, as the loader's decoder reads
/// it: a PIE for this machine, its entry loaded, an unwind table header, and no
/// program interpreter.
pub fn judge_elf(elf: &[u8]) -> Result<(), String> {
    let header = toyos_elf::FileHeader::parse(elf).map_err(|e| format!("toyos-elf refuses the header: {e:?}"))?;
    let machine = match super::qemu::SUITE_ARCH {
        toyos_build::arch::Arch::X86_64 => toyos_elf::Machine::X86_64,
        toyos_build::arch::Arch::Aarch64 => toyos_elf::Machine::Aarch64,
    };
    let layout = toyos_elf::Layout::parse(elf, machine).map_err(|e| format!("the loader's decoder refuses it: {e:?}"))?;
    if layout.eh_frame_hdr().is_none() {
        return Err("it has no unwind table header, which the driver asks for".to_string());
    }
    let table = header.program_headers(elf).map_err(|e| format!("its program headers: {e:?}"))?;
    let interp = (0..usize::from(header.phnum))
        .filter_map(|i| toyos_elf::header::ProgramHeader::parse(table, i))
        .any(|p| p.kind == toyos_elf::header::PT_INTERP);
    if interp {
        return Err("it names a program interpreter, which ToyOS does not have".to_string());
    }
    Ok(())
}

/// Gate: `hello.c`, compiled and linked by one clang invocation, runs on ToyOS.
pub fn c_hello(rust_bins: &[(String, Vec<u8>)]) -> Result<(), String> {
    let root = compile::repo_root();
    let c = compile::c_sysroot();
    let out = super::lane::dir().join("hello");
    let built = Command::new(&c.clang)
        .args(c.args())
        .arg("-O2")
        .arg(root.join(HELLO))
        .arg("-o")
        .arg(&out)
        .output()
        .map_err(|e| format!("run {}: {e}", c.clang.display()))?;
    if !built.status.success() {
        return Err(format!("clang could not build {HELLO}:\n{}", String::from_utf8_lossy(&built.stderr)));
    }
    let elf = fs::read(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    judge_elf(&elf)?;

    let config = root.join("tests/testcases");
    let c_tests = [("hello".to_string(), elf)];
    let mut qemu = QemuInstance::boot_with_options(&config, &c_tests, rust_bins, BootOptions::default());
    let result = qemu.run_test("test_c_hello", Duration::from_secs(60));
    if let Some(err) = &result.error {
        return Err(format!("{err}\n{}", result.stdout));
    }
    if result.exit_code != Some(0) {
        return Err(format!("hello exited {:?}:\n{}", result.exit_code, result.stdout));
    }
    if !result.stdout.lines().any(|l| l.trim_end() == SAYS) {
        return Err(format!("hello did not say {SAYS:?}:\n{}", result.stdout));
    }
    eprintln!("  [c_hello] {SAYS}");
    Ok(())
}
