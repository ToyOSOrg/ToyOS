---
status: open
kind: track
opened: 2026-10-01
---

# A panic is never an accident: the rule is decided, and seven crates enforce it

The owner has decided the rule below. Outside seven crates nothing enforces it
yet, and that is the present-state weakness.

| Tier | Rule |
|---|---|
| 1. Input boundaries: everything that reads bytes from outside the code's own trust (disk and partition formats, filesystems, ELF loading, microcode, network packets, USB descriptors, ACPI and PCI tables, system-call arguments, IPC messages, device registers and DMA data) | No panic at all. Clippy denies `indexing_slicing`, `arithmetic_side_effects`, `unwrap_used`, `expect_used`, `panic`, `unreachable` and truncating casts. Each parser's entry point also carries a link-time no-panic proof. |
| 2. The whole kernel | No implicit panic: the same lints are denied. A deliberate stop for a broken internal invariant stays (fail fast), but it is spelled out with its reason where a reviewer sees it, as an `#[expect(…, reason = …)]` or a named invariant. A type that makes the broken state unrepresentable is preferred over any stop. |
| 3. System services: init, logd, blockd, fsd, netd, compositor, soundd, sshd | The same rule as the kernel. |
| 4. Apps, ports, test code and build tooling | Normal Rust. |

**Today.**
- Seven crates carry the lints (`rg -l 'clippy::indexing_slicing' --glob '*.rs'`).
  `toyos-net-wire`, `-ip`, `-tcp`, `-udp`, `toyos-dhcp` and `toyos-gpt` forbid
  five of them outside tests, plus `as_conversions` in place of truncating
  casts. `toyos-transport` forbids four and denies `indexing_slicing` and
  `as_conversions`, with one `#[allow]` at `toyos-transport/src/queue.rs:21`.
  No crate denies `unreachable`.
- The kernel has 157 `.unwrap()`/`.expect(` calls on 154 lines
  (`rg -o '\.unwrap\(\)|\.expect\(' kernel/src | wc -l`; `rg -c` counts
  lines). Running `cargo clippy --target x86_64-unknown-none` in `kernel/`
  with the seven lints as warnings reports 2,008 findings at default features: 992
  `arithmetic_side_effects`, 408 `indexing_slicing`, 394
  `cast_possible_truncation`, 109 `expect_used`, 60 `panic`, 29 `unwrap_used`
  and 16 `unreachable`.
- `overflow-checks = true` is set in `[profile.toyos]` (`Cargo.toml:254`,
  `kernel/Cargo.toml:387`, `userland/Cargo.toml:85`), so an overflow stops the
  program. Overflow checks found the two crafted-ELF kernel panics, from
  `e_phoff = u64::MAX` and from a `.gnu.hash` `bloom_shift` of 32 or more.
  `issues/` holds neither. They are closed at `56aea566a:specs/known-issues.md:440`,
  and their tests are `toyos-elf/tests/crafted.rs:113` and
  `toyos-elf/tests/tables.rs:615`.
- The review of PR #653 at `38318d18b` found a length check at
  `toyos-microcode/src/lib.rs:203` that no test covers. It guards the panicking
  slice `table[EXT_HEADER..]` at `:213`.

**Constraints.**
- The lints do not see `assert!`, `copy_from_slice`, `split_at` or a panic
  inside a callee: on a crate holding the first three beside `a[3]` and
  `panic!`, they warned on those two alone.
- The no-panic proof needs unwinding: its README says "The attribute is useless
  in code built with `panic = "abort"`". Both kernel targets print
  `"panic-strategy": "abort"` from `rustc -Z unstable-options --print
  target-spec-json`, so the proof has to come from a host build.
- `--clippy` lints no userland program, because the `toyos` toolchain ships no
  clippy (`src/clippy.rs:7-8`).

**Stages, in order.**
1. Tier 1, one area at a time. An area's exit is its crate or kernel module
   denying the lints under `cargo run -- --clippy`. `toyos-elf` is
   `issues/design-debt/elf-domain-lint-line-not-yet-added.md`.
2. The link-time proof on the parser entry points: works or not, measured. The
   exit is a host build that refuses to link an entry point with a panicking
   path, or a `rejected` issue with the measurement.
3. The kernel, one module at a time, under `--clippy`. The stage ends when
   `kernel/src/main.rs` denies the lints for the whole crate.
4. The system services. A service's exit is its crate root denying the lints
   under a clippy run that reaches userland.
5. The rule goes into `.claude/agents/reviewer.md` beside "Edges".

**Mutation**: adding one `buf[i]` to a module that denies the lints turns
`cargo run -- --clippy` red.
