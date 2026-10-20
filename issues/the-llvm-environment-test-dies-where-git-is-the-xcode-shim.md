---
status: open
kind: tooling
opened: 2026-10-20
---

# The LLVM environment test dies where `git` is Xcode's shim

`llvm::tests::the_caller_s_environment_reaches_neither_the_build_nor_the_key`
(`build/llvm.rs`) reruns `llvm::tests::keyed_and_built` with `SDKROOT` set to
`/no/such.sdk`, among the other ambient names it proves never reach the LLVM
build. On a macOS host whose `/usr/bin/git` is Xcode's `xcrun` shim with no
standalone command line tools, the child's first `git` (`sysroot.rs`, the
fork's `.gitmodules` read) resolves through that `SDKROOT` and fails: `Failed
to determine realpath of '/no/such.sdk'`, `xcode-select: Failed to locate
'git'`. So `cargo run -- --ci host` reds its build-system step on such a host,
on `origin/main` at `0e6716fc0` as on any branch.

The test asserts what the *LLVM build* sees; the harness's own `git` reading
the caller's `SDKROOT` is the test's premise breaking, not the defect it
guards. Owner: the build track. Exit: the rerun's own `git` calls run without
the ambient names (or `git` is resolved before they are set), and the test
passes on a host whose `git` is the shim.
