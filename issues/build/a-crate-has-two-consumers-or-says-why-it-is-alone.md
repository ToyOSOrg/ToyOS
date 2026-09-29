---
status: open
kind: track
opened: 2026-09-29
---

# A crate has two consumers or says why it is alone

The tree has 95 packages. Its 44 `toyos-*` directories hold 47 of them. Of
those 47, 13 are a normal dependency of exactly one package, and 7 are a
dependency of none. Almost every single-consumer crate gives the same reason in
its manifest or module doc: as a crate, its tests run on the host. Measured on
2026-09-29, that reason does not hold:

- **A userland program host-tests its own modules.** `--ci host` already runs
  `cargo test --target <host>` in calc, logd, netd, pkg, soundd and sshd
  (`src/userlandhost.rs`). blockd, calc, pkg and terminal each carry a `lib`
  beside their `bin`.
- **A bare-target package can carry a host-tested library.** Take a package
  with a `src/lib.rs` (`cfg_attr(not(test), no_std)`, `forbid(unsafe_code)`)
  and a `no_std`/`no_main` bin, under a `.cargo/config.toml` naming
  `x86_64-unknown-none`. It builds both targets for the bare target, and
  `cargo test --lib --target aarch64-apple-darwin` runs the library's tests.
  With `[[bin]] test = false`, a plain `cargo test --target <host>` builds no
  binary. A host package that depends on it by path builds only the library.
- **The kernel binary itself runs a `#[cfg(test)]` test on the host** after
  four edits: `cfg_attr(not(test), no_main)`, the panic handler and global
  allocator taken out under test, and the three aarch64 entry assembly items
  taken out under test. Mach-O refuses their ELF local label and ADRP fixups.
  The test build also allows `dead_code` and `unused_imports`, and it builds
  clean in 3.4 s. On `x86_64-unknown-linux-gnu`, the test target compiles, and
  its object links as a PIE under `-z text`. Only libc, unwinder and
  allocator-shim symbols stay undefined. The full link there is unmeasured,
  because the CI host job runs on `macos-latest`.
- **`#![forbid(unsafe_code)]` holds at module scope.** In a module file, the
  inner attribute refuses an `unsafe` block and a local `allow` (E0453). A
  sibling module keeps its own `unsafe`.

So a crate with one consumer earns its boundary only for a reason other than
host tests. It may be built into the toolchain's sysroot. It may compile
another package's sources under another `cfg` (loom, a simulator, a host
differential). The crate boundary may itself be under test. Or an open track
may name its first consumer. The `cfg` case has a second reason: under one
`cargo test --workspace`, cargo unifies features, so a feature one package
turns on would leak into every other consumer's build.

**What a fold costs.**

- A library target shares its package's dependency list with the binary.
- The gated userland programs are not clippy-clean
  (`issues/build/userland-programs-are-never-linted.md`), so a crate folded
  into one leaves clippy's reach until that entry closes.
- `src/sourcegate.rs`'s `PURE_CRATES` rule is by directory, so it has to
  follow the modules.
- A clean bare kernel build takes 5.4 s, 4.3 s of it in the kernel crate. Each
  of the five kernel-only crates takes 0.09 to 0.32 s.

Stages. Each lands green on `--ci host` and `--build-only`:

1. Delete `toyos-userpin`. Nothing depends on it, and it names nothing the
   kernel defines. `munmap_reissues_read_window` is the gate. Check:
   `git grep toyos-userpin` is empty.
2. Close `issues/build/userland-programs-are-never-linted.md`. Every stage
   that folds into a userland program waits on it.
3. **The kernel's library.** `toyos-dma`, `-pci`, `-pcid`, `-proclife` and
   `-ps2` become modules of a `lib` target of `kernel/`, with crate-level
   `forbid(unsafe_code)`. It is a library, not tests inside the binary: the
   library cannot name the binary's items, which is the wall the five crates
   provided, and the binary needs no `cfg(test)`. `--ci host` tests it from
   `kernel/`, so that directory's `-Dwarnings` applies, and a clippy shape
   lints it on the host.
   proclife's six controls and pcid's `counting-allocator`
   (`issues/build/the-pcid-negative-control-runs-nowhere.md`) become kernel
   features in `CONTROLS`. Check: `-- --list` shows at least the five crates'
   156 tests. Every moved control reds. An `unsafe {}` planted in a moved
   module does not compile.
4. `toyos-symbols` splits. `locate` and `file_range` go into `toyos-elf`, and
   the name budget goes into the kernel's library. Check: `cargo tree` in
   `bootloader/` gains no package.
5. `toyos-acpi`, `-bootmap`, `-rootimage` and `-blackbox` become one
   `toyos-boot`. The four have the same consumers (the loader, the kernel, and
   the host reading the black box) and the same policy, so the merge adds no
   edge. Check: 136 tests and the doc-test are listed, and every loader and
   kernel clippy shape is green.
6. After stage 2, `toyos-mixer` folds into soundd, `toyos-desktop` into the
   compositor, and `toyos-mdns` into netd. Each becomes a module with
   `#![forbid(unsafe_code)]` at its root. Check: each program's listed tests
   grow by the folded crate's own count (55, 95 and 12), and
   `the_corpus_is_reproduced_bit_for_bit` is listed under `userland/soundd`.
7. `toyos-transport` folds into `toyos-blockring`, `toyos-net-wire` and
   `-tcp` merge into one `toyos-net`, and `toyos-sched/loom` folds into
   `kernel-loom`. Check: every moved control reds, and the listed counts sum
   (13+19, 273+358, 23+58, plus doc-tests).
8. **The gate.** `src/hostws.rs` reads every manifest in the tree and reds on
   a package whose only artifact is an `rlib` when it has fewer than two
   consumers and declares no exception under `[package.metadata.toyos]`. A
   binary or a `cdylib` is an artifact of its own and is not held to the rule.
   A consumer is a normal, build or dev path dependency, or a `#[path]`
   inclusion. The exceptions are a closed set: `sysroot`, `model`, `fixture`
   and `track`. A `track` exception names an existing `issues/` file. Check: a
   fixture with one consumer and no declaration reds.

The end state is 79 packages and 29 `toyos-*` directories. Six exceptions are
left:

- `model`: `kernel-loom`, `toyos-xhci/sim` and `toyos-libc-copies`.
- `fixture`: `tests/toyos-rust-tests/tls-multi-crate/dep`, whose crate boundary
  is what its test crosses.
- `sysroot`: `userland/libc`.
- `track`: `toyos-net`, with 17,318 lines, 631 tests and no consumer
  (`issues/design-debt/toyos-has-its-own-network-stack.md`).
