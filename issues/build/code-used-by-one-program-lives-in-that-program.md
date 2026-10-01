---
status: open
kind: track
opened: 2026-09-29
---

# Code used by one program lives in that program

A crate exists because two programs share it. What only the kernel uses is the
kernel package's, what only one userland program uses is that program's, and
shared crates with one subject are one crate (owner, 2026-09-29). No gate holds
the layout: step 2 writes it into `.claude/agents/reviewer.md`'s Fit line,
which until then puts a pure decision in a pure crate.

An input boundary is a crate of its own, whoever uses it: the no-panic
track (`issues/kernel/a-panic-is-never-an-accident.md`) forbids its tier 1 per
crate, and a crate that holds a tier-2 stop cannot forbid the set. A crate is
one when its own source decodes or refuses input from outside its trust:
hardware registers, firmware tables, disk bytes, network bytes, or what another
program sent, a syscall's arguments included. No step here moves or merges
one; each stays a crate under the no-panic track.

Every step lands green on `--ci host` and `--build-only`. Test counts are what
`cargo test -p <package> -- --list` lists today.

1. **Delete `toyos-userpin`.** It models the pin invariant and names nothing
   the kernel defines; `munmap_reissues_read_window` holds the kernel to it.
   Check: `git grep toyos-userpin -- ':!issues/'` is empty.
2. **The kernel's library.** `kernel/pure/` is the `kernel` package's lib, and
   its bin is `test = false`. `toyos-pcid` and `toyos-sched` move in.
   `kernel-loom` and `toyos-sched/loom` become `kernel/loom/`, and
   `toyos-sched/sim` `kernel/sim/`. The harness dev-depends on the kernel, and
   the build system does not depend on it. The library has no `tests/`, since
   an integration test builds the binary for the host. `--ci host` tests it
   with `sched-check`, the feature three scheduler tests need. The Fit line
   states this track's rule.
   Closes `issues/build/the-pcid-negative-control-runs-nowhere.md`.
   Check: the library lists at least 101 tests, `kernel/loom` 79 and
   `kernel/sim` 53, and `--clippy` lints the library's tests on the host.
   Every moved control, and pcid's `counting-allocator`, reds with its verdict,
   and `declared_model_controls` reads the kernel's manifest and every one in
   the host workspace, not a list. An `unsafe {}` planted in a module that was
   `forbid(unsafe_code)` does not compile. `cargo tree -e normal -p
   toyos-build` names no `kernel`.
3. **libc's host test moves to the tests.** `toyos-libc-copies` moves to
   `tests/libc-arch/`.
   Check: `--ci host` runs there every test the package lists today, and
   `--clippy` lints them.

**Exit:** no directory this file names as moved or merged still exists.
