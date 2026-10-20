---
status: open
kind: tooling
opened: 2026-10-20
---

# The LLVM environment test breaks the host's git on macOS, so `--ci host` is red on every macOS host

`build/llvm.rs`'s `the_caller_s_environment_reaches_neither_the_build_nor_the_key`
re-runs `llvm::tests::keyed_and_built` in a child holding every `AMBIENT`
name, among them `SDKROOT=/no/such.sdk`. Before the child reaches the build
under test, its key reads `.gitmodules` through `sysroot::git_bytes`, which
runs the host's `git` with that environment. On macOS `/usr/bin/git` is an
xcrun wrapper that resolves the real tool through `SDKROOT`. The wrapper
fails ("Failed to determine realpath of '/no/such.sdk'", "xcode-select: Failed
to locate 'git'"), so `cargo test --lib` panics at `build/sysroot.rs:1215`
and the test fails every time. `cargo run -- --ci host` was red
1 of 86 on a macOS arm64 host at 64e6e404e for this reason alone; the
evidence is at
https://github.com/ToyOSOrg/evidence/tree/ab436d34368b99cdeb8d46a333be7ab2c8ecbb87/pr-952/ci
(`ci-host.log`, `llvm-env-test.log`).

Owner: the build track (`build/`).

Exit: the test establishes its claim without breaking the host's own tools,
for example by resolving git before the environment is scrubbed, or by
applying the ambient environment only to the child under test.
