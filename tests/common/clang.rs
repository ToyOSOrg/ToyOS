//! A C program compiled and linked by the toolchain's clang — the ToyOS driver
//! `ToyOSOrg/llvm-project` carries — judged as the loader and a second reader
//! see the file, and then run on ToyOS.

use std::fs;
use std::process::Command;
use std::time::Duration;

use super::compile;
use super::qemu::{BootOptions, QemuInstance};

/// The program, beside the corpus it is not part of.
const HELLO: &str = "tests/testcases/hello.c";
/// What it prints, all of which its own arithmetic and libc produce.
const SAYS: &str = "hello from clang, on ToyOS: 6 * 7 = 42";

/// What an image the ToyOS driver links must be, read two ways that share no
/// code: `toyos-elf`, the decoder the kernel's loader runs, and LLVM's
/// `llvm-readobj`. The loader accepts it; both see a position-independent
/// executable for this machine with the same entry and the same loadable
/// segments; and it names no program interpreter, which ToyOS does not have.
pub fn judge_elf(elf: &[u8], readobj: &std::path::Path, path: &std::path::Path) -> Result<(), String> {
    let header = toyos_elf::FileHeader::parse(elf).map_err(|e| format!("toyos-elf refuses the header: {e:?}"))?;
    let (machine, em) = match super::qemu::SUITE_ARCH {
        toyos_build::arch::Arch::X86_64 => (toyos_elf::Machine::X86_64, "EM_X86_64"),
        toyos_build::arch::Arch::Aarch64 => (toyos_elf::Machine::Aarch64, "EM_AARCH64"),
    };
    if header.machine != machine {
        return Err(format!("toyos-elf reads machine {:?}, not {machine:?}", header.machine));
    }
    let layout = toyos_elf::Layout::parse(elf, machine).map_err(|e| format!("the loader's decoder refuses it: {e:?}"))?;

    let read = Command::new(readobj)
        .args(["--file-headers", "--program-headers"])
        .arg(path)
        .output()
        .map_err(|e| format!("run {}: {e}", readobj.display()))?;
    if !read.status.success() {
        return Err(format!("llvm-readobj refuses it: {}", String::from_utf8_lossy(&read.stderr)));
    }
    let text = String::from_utf8_lossy(&read.stdout);
    let field = |name: &str| {
        text.lines()
            .find_map(|l| l.trim().strip_prefix(name))
            .map(str::trim)
            .ok_or_else(|| format!("llvm-readobj printed no {name:?}:\n{text}"))
    };
    if !field("Type:")?.starts_with("SharedObject") {
        return Err(format!("llvm-readobj reads type {}, and a PIE is ET_DYN", field("Type:")?));
    }
    if !field("Machine:")?.starts_with(em) {
        return Err(format!("llvm-readobj reads machine {}", field("Machine:")?));
    }
    let entry = field("Entry:")?;
    let entry = u64::from_str_radix(entry.trim_start_matches("0x"), 16).map_err(|e| format!("entry {entry:?}: {e}"))?;
    if entry != header.entry {
        return Err(format!("llvm-readobj reads entry {entry:#x} and toyos-elf {:#x}", header.entry));
    }
    let loads = text.matches("Type: PT_LOAD").count();
    if loads != layout.segments().len() {
        return Err(format!(
            "llvm-readobj counts {loads} PT_LOAD and the loader's decoder {}",
            layout.segments().len()
        ));
    }
    if text.contains("PT_INTERP") {
        return Err(format!("it names a program interpreter, which ToyOS does not have:\n{text}"));
    }
    if !text.contains("PT_GNU_EH_FRAME") {
        return Err(format!("it has no unwind table header, which the driver asks for:\n{text}"));
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
    judge_elf(&elf, &c.readobj, &out)?;

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
