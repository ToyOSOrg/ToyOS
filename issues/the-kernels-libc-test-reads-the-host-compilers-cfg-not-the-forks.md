---
status: open
kind: tooling
opened: 2026-10-03
---

# The kernel's libc test reads the host compiler's `cfg`, not the fork's that builds the kernel

`build::tests::the_kernel_resolves_no_libc_for_either_target` (`src/build.rs`)
runs `cargo tree --target <t>` for each kernel target with whatever `cargo`
and `rustc` the host gate runs: the host's default, stable on CI, whose host
job installs no ToyOS toolchain (`src/ci.rs`'s `host`). Cargo decides each
target-gated edge against that compiler's `rustc --print cfg`. The kernel
builds with the fork, whose `cfg` for the kernel targets only
`assert_kernel_is_softfloat` (`src/build.rs`) asks.

The two answers differ. `rustc --print cfg --target <t>`, stable `1.98.1`
against the fork's `1.99.0-dev`, every run exiting 0: on both
`x86_64-unknown-none` and `aarch64-unknown-none-softfloat` the fork prints
every line stable prints, and adds `overflow_checks`, `ub_checks`,
`fmt_debug`, `relocation_model`, `target_has_threads`,
`target_object_format`, and the unstable `target_has_atomic*` and
`target_has_reliable_*` names; on x86_64 it adds `target_feature="x87"` too.
Both print the same `target_os` and `target_arch` and no `unix`, the names
`dlmalloc` 0.2.13 gates its `libc` and `windows-sys` edges on, so the test's
verdict is the fork's today. An edge gated on a name only the fork
prints resolves one way in the kernel's build and the other in the test.

Owner: the orchestrator.

**Exit**: the test resolves the kernel's dependencies against the `cfg` of
the compiler that builds the kernel, or it is deleted, as
`issues/toyos-has-its-own-allocator.md`'s exit deletes it.
