---
status: open
kind: track
opened: 2026-10-01
---

# A panic is never an accident: the owner's rule, not yet enforced

| Tier | Rule |
|---|---|
| 1. Input boundaries | No panic at all: the set below is forbidden. Where the build allows, each parser's entry point also carries a link-time no-panic proof. |
| 2. The kernel | No implicit panic: the set is denied. A deliberate stop for a broken internal invariant stays, as an `#[expect(…, reason = "…")]` whose reason names the invariant. |
| 3. System services | The same rule as the kernel. |
| 4. Apps, ports, test code and build tooling | Normal Rust. |

**The set**:
- indexing and slicing: `clippy::indexing_slicing`, and `clippy::string_slice`,
  because the first does not fire on slicing a `str`;
- arithmetic: `clippy::arithmetic_side_effects`. `[profile.toyos]` sets
  `overflow-checks`, so arithmetic is a panicking form;
- `clippy::unwrap_used`, `clippy::expect_used`, `clippy::panic`,
  `clippy::unreachable`, `clippy::todo`, `clippy::unimplemented`,
  `clippy::panic_in_result_fn`;
- lossy casts: `clippy::cast_possible_truncation`, `clippy::cast_possible_wrap`,
  `clippy::cast_sign_loss` and `clippy::cast_precision_loss`. A lossy `as`
  does not panic; it changes the value silently, and `CLAUDE.md`'s "Fail fast"
  puts panics over silent degradation. For tiers 1 to 3 that overrides their
  rejection in `issues/build/clippy-stage-two-is-lints-one-at-a-time.md`;
- standard functions: `clippy::disallowed_methods` naming `slice::split_at`,
  `slice::split_at_mut`, `slice::copy_from_slice` and
  `slice::clone_from_slice`, and `clippy::disallowed_macros` naming
  `core::assert`, `core::assert_eq` and `core::assert_ne`.

**Today.** No crate carries the set. In the kernel package alone, `cargo
clippy --target x86_64-unknown-none` with the set as warnings reports 2,282
findings, 984 of them `arithmetic_side_effects`, 405 `indexing_slicing`, 423
lossy casts and 205 `disallowed_macros`.

**Constraints.**
- The set does not see a shift by a variable amount, `pow` or `abs`, though
  each panics under `overflow-checks`; nor a standard function it does not
  name, nor a panic inside a callee.
- `clippy::allow_attributes` checks outer attributes alone: an inner
  `#![allow(…, reason = "…")]` passes it and `allow_attributes_without_reason`.
- Cargo's `[lints]` reaches a crate's `#[cfg(test)]` code, and its `forbid`
  refuses the `cfg_attr(test, allow(…))` that would free it. `cargo clippy
  --lib -- -F <lint>` reaches the library alone, and refuses an inner
  `#[expect]` of the lint. Each reaches a whole crate, never one module of it.
- `disallowed_methods` and `disallowed_macros` read the nearest `clippy.toml`
  alone, and the root's reaches every crate beneath it, tier 4 included.
- The no-panic proof needs unwinding: its README says "The attribute is useless
  in code built with `panic = "abort"`". Both kernel targets print
  `"panic-strategy": "abort"` from `rustc -Z unstable-options --print
  target-spec-json`, so the proof has to come from a host build.
- `--clippy` lints no userland program, because the `toyos` toolchain ships no
  clippy (`src/clippy.rs:7-8`).

**Stages, in order.**
1. Tier 1, one crate at a time; an input boundary inside the kernel is moved
   into a crate of its own first. The first exit is one declaration of the set
   that every tier-1 crate's library is linted under in place of its own copy,
   its tests left at tier 4. An area's exit is its crate forbidding the set
   under `cargo run -- --clippy`; `forbid` refuses any inner `#[allow]` or
   `#[expect]` of it.
2. The link-time proof on the parser entry points: works or not, measured. The
   exit is a step of `cargo run -- --ci host` whose host build refuses to link
   an entry point with a panicking path, or a `rejected` issue with the
   measurement.
3. The kernel, one module at a time. The stage ends when `kernel/src/main.rs`
   and every crate of this tree `kernel/Cargo.toml` links, unless stage 1
   already forbids the set in it, deny the set and forbid
   `clippy::allow_attributes` and `clippy::allow_attributes_without_reason`
   under `--clippy`, and a step of `--ci host` refuses an inner `#![allow]`,
   and an `#[expect]` of the set on a `mod`, on an `impl` or as an inner
   `#![expect]`, any of which passes every finding beneath it: then every
   exception is an `#[expect(…, reason = "…")]`, and a bare `#[allow]` fails
   the gate.
4. The system services. The first exit is a userland shape in `src/clippy.rs`;
   a service's exit is its crate root under stage 3's attributes.
