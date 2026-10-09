---
status: open
kind: defect
opened: 2026-10-09
---

# The bootstrap's own build cannot start `rust-objcopy` on an Apple host

Run 37834436612's `portability-macos` (a hosted `macos-26-arm64` runner, head
`c952668c9`, before #769) kept the reports macOS wrote during the job. 22 of
them are of `rust-objcopy`, each ended by dyld at launch, `EXC_CRASH`,
`SIGABRT`, "terminated at launch", inside `cargo run -- --build-only`, which
went on and succeeded:

| when | the log's line before them | reports | dyld's reason |
|---|---|---|---|
| 19:53:54 to 19:54:19Z | `19:53:48 Building bootstrap`, the first | 13 | `Library not loaded: @rpath/libLLVM.dylib` |
| 21:12:19 to 21:12:29Z | `21:12:05 Building bootstrap`, the second, after LLVM was built and installed | 9 | `Symbol not found: __ZN4llvm18format_object_base4homeEv`, expected in a `libLLVM.dylib` |

rustc strips a Darwin binary by running `rust-objcopy`
(`src/toolchain.rs`, above `a_compiler_build_finds_the_llvm_s_objcopy_where_rustc_strips_with_it`).
Both moments are the downloaded beta compiler building bootstrap itself. In
the first nothing it finds as `rust-objcopy` has an LLVM library to load; in
the second it finds one that loads a `libLLVM.dylib` without a symbol it
needs, which reads as the tree's `llvm-objcopy`, put on `PATH` for the
stage-1 compiler, started by the beta compiler against the beta's own
library. That is a reading of the two reasons and the two times; 8 of the 22
name `rustc` as the process that started them and the rest a process that
had exited, and no run has shown which file each launch was.

A developer's Mac does the same. The Apple-silicon Mac this tree is developed
on holds 18 reports of `rust-objcopy`, 1 of 4 October, 12 of 7 October and 5
of 8 October, every one `Library not loaded`, none `Symbol not found`. Two
hosts do it, so it is a defect of the `PATH` the compiler build is given.

Not known: whether bootstrap's binaries are left unstripped by this and
whether anything reads the difference; and whether #769, which moved the
toolchain into a store and landed on 8 October, changed it.

Nobody holds it. It is in how a compiler build finds `rust-objcopy`
(`src/toolchain.rs`).

**Exit**: a compiler built from nothing by `cargo run -- --build-only` on an
Apple host, at a tree that has #769, that leaves no `rust-objcopy` report.
