---
status: open
kind: track
opened: 2026-09-29
---

# Code used by one program lives in that program

A crate exists because two programs share it. What only the kernel uses is the
kernel package's, what only one userland program uses is that program's, and
shared crates with one subject are one crate (owner, 2026-09-29). No gate holds
the layout: step 3 writes it into `.claude/agents/reviewer.md`'s Fit line,
which until then puts a pure decision in a pure crate.

An input boundary is a crate of its own, whoever uses it: the no-panic
track (`issues/kernel/a-panic-is-never-an-accident.md`) forbids its tier 1 per
crate, and a crate that holds a tier-2 stop cannot forbid the set. That track
says which crates are boundaries, and no step here moves one. `toyos-userbound`,
`-dma`, `-pci`, `-acpi`, `-transport`, `-blockring` and `-dns` say so at their
roots, `-ps2` decodes a device's wire, and the other network crates read the
network: they stay where they are.

An open track plans a single-program crate this one folds back:
`toyos-supervisor` (`issues/isolation/the-supervisor-is-host-tested-and-owns-the-stop.md`).
They are reconciled before step 5.

Every step lands green on `--ci host` and `--build-only`. Test counts are what
`cargo test -p <package> -- --list` lists today.

1. **Delete `toyos-userpin`.** It models the pin invariant and names nothing
   the kernel defines; `munmap_reissues_read_window` holds the kernel to it.
   Check: `git grep toyos-userpin -- ':!issues/'` is empty.
2. **Lint userland first.** Close
   `issues/build/userland-programs-are-never-linted.md`: steps 3 to 5 move code
   out of the host workspace's clippy. Check: that issue's planted finding reds.
3. **The kernel's library.** `kernel/pure/` is the `kernel` package's lib, and
   its bin is `test = false`. `toyos-pcid`, `-proclife`, `-sched`, `-xhci`,
   `-gicv3`, `-cpuvuln` and `toyos-symbols`' name budget move in; `locate` and
   `file_range` go to `toyos-elf`. `kernel-loom` and `toyos-sched/loom` become
   `kernel/loom/`, and the two simulators `kernel/sim/`. The harness
   dev-depends on the kernel, and the build system does not depend on it. The
   library has no `tests/`, since an integration test builds the binary for the
   host. `--ci host` tests it with `sched-check`, the feature three scheduler
   tests need. The Fit line states this track's rule.
   Closes `issues/build/the-pcid-negative-control-runs-nowhere.md`.
   Check: the library lists at least 345 tests, and it and `toyos-elf` gain
   `toyos-symbols`' 9 between them. `kernel/loom` lists 79 and `kernel/sim` 99.
   Every moved control, and pcid's `counting-allocator`, reds with its verdict,
   and `declared_model_controls` reads the kernel's manifest and every one in
   the host workspace, not a list. An `unsafe {}` planted in a module that was
   `forbid(unsafe_code)` does not compile. `cargo tree -e normal -p
   toyos-build` names no `kernel`.
4. **Shared crates merge by subject.** `toyos-boot` is `toyos-bootmap`,
   `-rootimage`, `-blackbox`, `-tco` and `-quiesce`. `toyos-log` is
   `toyos-elide` and `-logstream`. `toyos-manifest` takes in `toyos-swap`, and
   `toyos-fat32-check` moves to `toyos-fat32/check/`.
   Check: the merged crates list at least 118, 26, 26 and 67 tests, and every
   loader and kernel clippy shape is green. `cargo tree` in `bootloader/` names
   no package it did not name before, except `toyos-boot`.
5. **Userland code goes home.** `toyos-mixer` moves into soundd and
   `toyos-desktop` into the compositor. `filepicker-api` becomes filepicker's
   library, and `toyos-inspect` inspect's library. `toyos-libc-copies` moves
   to `tests/libc-arch/`. Userland's host tests are built at opt-level 2, since
   the mixer's tests take 9.94 s at opt-level 0 and 1.00 s at 2.
   Check: soundd lists 55 more tests, the compositor 95 and inspect 21, and
   `the_corpus_is_reproduced_bit_for_bit` is listed under soundd.
   `--build-only` builds the guest tests against inspect's library.

**Exit:** no directory this file names as moved or merged still exists.
