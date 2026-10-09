//! The C++ runtime of a C sysroot (`clang::CSysroot`): LLVM's libc++,
//! libc++abi and libunwind, built by CMake and n2 (`n2::ninja`) from the
//! runtimes' sources the LLVM carries (`src/llvm.rs`), with that LLVM's clang,
//! against the C library the sysroot already holds (`src/libc.rs`).
//!
//! **One archive, `lib/libc++.a`, is the whole runtime**: libc++abi is linked
//! into it and libunwind into that, so the `-lc++` the ToyOS driver names for a
//! C++ link is all it needs, and its headers are `include/c++/v1`, where the
//! driver looks. [`OPTIONS`] is the configuration, each choice with its reason.

use std::fs;
use std::path::Path;
use std::process::Command;

use crate::clang::CSysroot;

/// What of `src/llvm-project` the runtimes' build reads: the runtimes, the CMake
/// modules they share with LLVM, and LLVM's libc, whose number parsing libc++
/// compiles in.
pub(crate) const SOURCES: [&str; 7] = ["runtimes", "cmake", "llvm/cmake", "libunwind", "libcxxabi", "libcxx", "libc"];

/// The runtimes' CMake options beyond the target's, which the C sysroot's
/// toolchain file names (`clang::CMAKE`), and the tools'.
pub(crate) const OPTIONS: [(&str, &str); 18] = [
    ("CMAKE_BUILD_TYPE", "Release"),
    ("LLVM_ENABLE_RUNTIMES", "libunwind;libcxxabi;libcxx"),
    ("LLVM_INCLUDE_TESTS", "OFF"),
    ("LLVM_INCLUDE_DOCS", "OFF"),
    // The C library is a static archive, so the C++ runtime is one too.
    ("LIBUNWIND_ENABLE_SHARED", "OFF"),
    ("LIBCXXABI_ENABLE_SHARED", "OFF"),
    ("LIBCXX_ENABLE_SHARED", "OFF"),
    ("LIBCXXABI_ENABLE_STATIC_UNWINDER", "ON"),
    ("LIBCXX_ENABLE_STATIC_ABI_LIBRARY", "ON"),
    ("LIBUNWIND_INSTALL_LIBRARY", "OFF"),
    ("LIBUNWIND_INSTALL_HEADERS", "OFF"),
    ("LIBCXXABI_INSTALL_LIBRARY", "OFF"),
    ("LIBCXX_INSTALL_MODULES", "OFF"),
    // The C library runs `thread_local` destructors (`__cxa_thread_atexit_impl`).
    ("LIBCXXABI_HAS_CXA_THREAD_ATEXIT_IMPL", "ON"),
    // The C library has no `dladdr`, so libunwind names no function it unwinds.
    ("LIBUNWIND_ADDITIONAL_COMPILE_FLAGS", "-D_LIBUNWIND_USE_DLADDR=0"),
    // `open` refuses every directory, so no descriptor names one for `openat`
    // and `unlinkat` to resolve against: `remove_all` walks a directory
    // iterator, as libc++'s Windows does
    // (`issues/remove-all-follows-a-link-swapped-in-mid-walk.md`).
    ("LIBCXX_ADDITIONAL_COMPILE_FLAGS", "-DREMOVE_ALL_USE_DIRECTORY_ITERATOR"),
    ("LIBCXX_INCLUDE_BENCHMARKS", "OFF"),
    ("LIBCXX_INCLUDE_TESTS", "OFF"),
];

/// Build `c`'s C++ runtime from the runtimes' `sources` into it, which holds
/// the C library already, under `ninja`, in `scratch`, which it removes when
/// done.
pub fn build(c: &CSysroot, sources: &Path, ninja: &Path, scratch: &Path) {
    eprintln!("Building the C++ runtime for {} under {}", c.target, ninja.display());
    if scratch.exists() {
        fs::remove_dir_all(scratch).unwrap_or_else(|e| panic!("remove {}: {e}", scratch.display()));
    }
    fs::create_dir_all(scratch).unwrap_or_else(|e| panic!("create {}: {e}", scratch.display()));
    // What CMake runs after archiving: LLVM's archiver is `ranlib` when run by
    // that name, which is all the link LLVM installs as `llvm-ranlib` is.
    let ranlib = scratch.join("llvm-ranlib");
    std::os::unix::fs::symlink(&c.ar, &ranlib)
        .unwrap_or_else(|e| panic!("symlink {} -> {}: {e}", ranlib.display(), c.ar.display()));
    let path = |p: &Path| p.display().to_string();
    let mut definitions: Vec<(String, String)> = OPTIONS.iter().map(|(n, v)| (n.to_string(), v.to_string())).collect();
    definitions.push(("CMAKE_TOOLCHAIN_FILE".into(), path(&c.cmake_toolchain())));
    for lang in ["C", "CXX", "ASM"] {
        definitions.push((format!("CMAKE_{lang}_COMPILER"), path(&c.clang)));
    }
    definitions.push(("CMAKE_AR".into(), path(&c.ar)));
    definitions.push(("CMAKE_RANLIB".into(), path(&ranlib)));
    definitions.push(("CMAKE_INSTALL_PREFIX".into(), path(&c.dir)));
    definitions.push(("CMAKE_MAKE_PROGRAM".into(), path(ninja)));

    let mut configure = Command::new("cmake");
    configure.args(["-G", "Ninja", "-Wno-dev", "-S"]).arg(sources.join("runtimes")).arg("-B").arg(scratch);
    configure.args(definitions.iter().map(|(name, value)| format!("-D{name}={value}")));
    run(configure, "configuration", c.target);
    let mut install = Command::new(ninja);
    install.arg("-C").arg(scratch).arg("install");
    run(install, "build", c.target);

    let archive = c.dir.join("lib/libc++.a");
    assert!(archive.is_file(), "the C++ runtime for {} was built, and installed no {}", c.target, archive.display());
    fs::remove_dir_all(scratch).unwrap_or_else(|e| panic!("remove {}: {e}", scratch.display()));
}

/// Run `command` seeing nothing of this process's environment but what the
/// LLVM build does (`llvm::clear`), and refuse its failure with all it said.
fn run(mut command: Command, what: &str, target: &str) {
    crate::llvm::clear(&mut command);
    let out = command.output().unwrap_or_else(|e| panic!("run {command:?}: {e}"));
    assert!(
        out.status.success(),
        "the C++ runtime's {what} for {target} failed ({}):\n{}{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}
