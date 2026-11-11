---
status: open
kind: tooling
opened: 2026-09-30
---

# The C++ runtime is rebuilt with every sysroot

`build/sysroot.rs` builds each target's libc++, libc++abi and libunwind into the
C sysroot after libc, so every change the sysroot key sees, an edit to
`sdk/abi/src`, `sdk/toyos/src` or libc's `src/` among them, configures and
builds both targets' runtimes again. One item added to
`sdk/abi/src/clock.rs`, measured on the `wt/toyos-rebuild` branch, cost 27 s
and 38 s at load 41, and 47 s and 54 s at load 68.

The runtime is a function of the LLVM key, libc's `include/` and
`build/libcxx.rs` only if no configure probe links: the runtimes' `try_compile`
builds an executable by default, which links `CMAKE_SYSROOT`'s
`libtoyos_c.a`, so today libc's archive decides probe answers too.
`CMAKE_TRY_COMPILE_TARGET_TYPE=STATIC_LIBRARY` stops the link, but a probe
that only a link answers (`check_function_exists`, `check_library_exists`)
then answers yes for anything: its configure's results are owed a comparison
against today's before it is set.

**Exit**: with the probes shown not to need a link, the runtime is a store
product of its own, keyed on the LLVM key, libc's `include/` and
`build/libcxx.rs`, and a sysroot build takes it (the store is #629's): one
item added to `sdk/abi/src/clock.rs` after a build, and the next
`cargo run -- --build-only` prints no `Building the C++ runtime`.
