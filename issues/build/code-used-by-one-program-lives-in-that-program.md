---
status: open
kind: track
opened: 2026-09-29
---

# Code used by one program lives in that program

A crate exists because two programs share it. What only the kernel uses is the
kernel package's, what only one userland program uses is that program's, and
shared crates with one subject are one crate. No gate holds the layout; it is
kept by review (owner, 2026-09-29).

Two open tracks plan single-program crates this one folds back:
`toyos-supervisor` (`issues/isolation/the-supervisor-is-host-tested-and-owns-the-stop.md`)
and the stack's `toyos-net-shard` and `toyos-net-testnet`
(`issues/design-debt/toyos-has-its-own-network-stack.md`). They are reconciled
before step 5.

Every step lands green on `--ci host` and `--build-only`. Test counts are what
`cargo test -p <package> -- --list` lists today.

1. **Delete `toyos-userpin`.** It models the pin invariant and names nothing
   the kernel defines; `munmap_reissues_read_window` holds the kernel to it.
   Check: `git grep toyos-userpin` is empty.
2. **Lint userland first.** Close
   `issues/build/userland-programs-are-never-linted.md`: steps 3 to 5 move code
   out of the host workspace's clippy. Check: that issue's planted finding reds.
3. **The kernel's library.** `kernel/pure/` is the `kernel` package's lib, and
   its bin is `test = false`. `toyos-dma`, `-pci`, `-pcid`, `-proclife`,
   `-ps2`, `-sched`, `-xhci`, `-userbound`, `-gicv3`, `-cpuvuln` and
   `toyos-symbols`' name budget move in; `locate` and `file_range` go to
   `toyos-elf`. `kernel-loom` and `toyos-sched/loom` become `kernel/loom/`,
   and the two simulators `kernel/sim/`. The harness dev-depends on the
   kernel, and the build system does not depend on it. The library has no `tests/`, since an integration
   test builds the binary for the host. `--ci host` tests it with
   `sched-check`, the feature three scheduler tests need.
   Closes `issues/build/the-pcid-negative-control-runs-nowhere.md`.
   Check: the library lists at least 495 tests, and it and `toyos-elf` gain
   `toyos-symbols`' 9 between them. `kernel/loom` lists 79 and `kernel/sim` 99.
   Every moved control, and pcid's `counting-allocator`, reds with its verdict.
   An `unsafe {}` planted in a module that was `forbid(unsafe_code)` does not
   compile. `cargo tree -e normal -p toyos-build` names no `kernel`.
4. **Shared crates merge by subject.** `toyos-boot` is `toyos-acpi`,
   `-bootmap`, `-rootimage`, `-blackbox`, `-tco` and `-quiesce`. `toyos-log`
   is `toyos-elide` and `-logstream`. `toyos-block` is `toyos-blockhold`,
   `-blockring` and `-transport`. `toyos-manifest` takes in `toyos-swap`, and
   `toyos-fat32-check` moves to `toyos-fat32/check/`.
   Check: the merged crates list at least 170, 26, 45, 26 and 67 tests. The
   blockring and transport controls red, and every loader and kernel clippy
   shape is green. `cargo tree` in `bootloader/` names no package it did not
   name before, except `toyos-boot`.
5. **Userland code goes home.** `toyos-mixer` moves into soundd and
   `toyos-desktop` into the compositor. `toyos-dns`, `-mdns`, `-net-wire`,
   `-net-tcp`, `-net-ip`, `-net-udp` and `toyos-dhcp` become netd's library,
   `filepicker-api` filepicker's library, and `toyos-inspect` inspect's
   library. `toyos-libc-copies` moves to `tests/libc-arch/`. Userland's host
   tests are built at opt-level 2, since soundd's tests with the mixer take
   34.31 s at 0 and 3.07 s at 2.
   Check: soundd lists 55 more tests, the compositor 95, netd 1105 and inspect
   21, and `the_corpus_is_reproduced_bit_for_bit` is listed under soundd.
   `--build-only` builds the guest tests against netd's and inspect's
   libraries.

**Exit:** no directory this file names as moved or merged still exists.
