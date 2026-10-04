---
status: open
kind: defect
opened: 2026-10-04
---

# The sysroot's `core` and `alloc` carry the debug assertions of a profile written for compiler contributors

`std_config` (`src/sysroot.rs`) writes `profile = "compiler"` into the
bootstrap configuration every ToyOS library is built under: the kernel's and
the loader's `core` and `alloc` (`Libraries::Freestanding`) as well as every
program's `std`. In the `rust/` fork at `a0d44493`, that profile
(`src/bootstrap/defaults/bootstrap.compiler.toml`) sets
`rust.debug-assertions = true`; bootstrap's `std_debug_assertions` falls back
to it when `rust.debug-assertions-std` is unset
(`src/bootstrap/src/core/config/config.rs`), which `std_config` leaves it, and
passes it to the library crates' cargo profile
(`src/bootstrap/src/core/builder/cargo.rs`). Upstream's distributed libraries
are built without. The libraries' assertions were never chosen: `std_config`'s
doc comment takes the profile so the libraries are built as the compiler was.
The guest profile keeps debug assertions on in ToyOS's own code "because
fail-fast beats speed here" (`kernel/Cargo.toml`, `userland/Cargo.toml`, the
root `Cargo.toml`); whether the libraries should too is the decision nobody
made.

So every kernel and program ships `alloc`'s and `core`'s `debug_assert!`s,
`RawVecInner::finish_grow`'s alignment check among them
(`library/alloc/src/raw_vec/mod.rs`), and the precondition checks `ub_checks`
gates in the code compiled into those crates. The same profile's
`frame-pointers = true` reaches them as `-Cforce-frame-pointers=true`.

**Measured.** A link-time no-panic proof: a `no_std` binary for
`x86_64-unknown-none` whose panic handler calls a symbol nobody defines,
linking a fallible parser. The binary with no parser links in every arm. Each
of a `Vec::push`, a `handle_alloc_error`, an over-wide shift and an
out-of-range index fails to link in every arm without `-Zub-checks=no`; with
it, only the `handle_alloc_error` control ran, against stable's and nightly's
libraries, and failed, and the ToyOS sysroot's rows ran no failing control.
The parser, built with the
binary's own debug assertions off, links against stable 1.98.1's and nightly's
upstream libraries and fails against a ToyOS sysroot, at `opt-level = 2` and
under fat LTO alike. In the parser's arm that writes into a vector's spare
capacity and sets its length in `unsafe`, lld's `--why-live` names `core::panicking::panic` and `panic_fmt`,
live through `RawVecInner::finish_grow` alone; in its default arm
`alloc::raw_vec::handle_error` and `handle_alloc_error` are live beside them,
and against upstream's libraries nothing is. Under the guest
profile with `-Zub-checks=no` it links against stable's and fails against
ToyOS's. The matrix, the `--why-live` logs and the spike's source are a
comment on the pull request that added this file. So stage 2 of
`issues/kernel/a-panic-is-never-an-accident.md` can prove a parser panic-free
against an upstream library on the host, and against none the kernel links.

**Not measured**: the `ub_checks` share of what the proof found, which no log
attributes; what the libraries' assertions, checks and frame pointers cost a
kernel or a program in time or in image bytes, on QEMU or the T14.

Owner: the orchestrator. Stage 2 of
`issues/kernel/a-panic-is-never-an-accident.md` exits on a host step against
upstream libraries, so it does not wait on this file.

**Exit**: `std_config` sets `rust.debug-assertions-std` by a line of its own,
so the compiler profile decides nothing about the libraries' assertions; and
either it is `false` and the proof's parser, built with its own debug
assertions off, links against the sysroot that configuration builds, or a
ruling keeps it `true` and this file becomes `rejected`, citing the ruling.
