---
status: open
kind: track
opened: 2026-10-01
---

# A panic is never an accident: the owner's rule, not yet enforced

| Tier | Rule |
|---|---|
| 1. Input boundaries | No panic at all: the set below is forbidden. Where the build allows, each parser's entry point also carries a link-time no-panic proof. |
| 2. The kernel | No implicit panic: the set is denied. A deliberate stop for a broken internal invariant stays (fail fast), as an `#[expect(…, reason = "…")]` whose reason names the invariant. A type that makes the broken state unrepresentable is preferred over any stop. |
| 3. System services | The same rule as the kernel. |
| 4. Apps, ports, test code and build tooling | Normal Rust. |

**The set**, every name checked against clippy 0.1.98 (`48a229ceae`):
- indexing and slicing: `clippy::indexing_slicing`, and `clippy::string_slice`,
  because the first does not fire on slicing a `str`;
- arithmetic: `clippy::arithmetic_side_effects`. `[profile.toyos]` sets
  `overflow-checks`, so arithmetic is a panicking form;
- `clippy::unwrap_used`, `clippy::expect_used`, `clippy::panic`,
  `clippy::unreachable`, `clippy::todo`, `clippy::unimplemented`,
  `clippy::panic_in_result_fn`;
- truncating casts: `clippy::cast_possible_truncation`. A truncating `as` does
  not panic; the owner names it, and for tiers 1 to 3 that overrides its
  rejection in `issues/build/clippy-stage-two-is-lints-one-at-a-time.md`;
- standard functions: `clippy::disallowed_methods` naming `slice::split_at` and
  `slice::copy_from_slice`, and `clippy::disallowed_macros` naming
  `core::assert`, `core::assert_eq` and `core::assert_ne`.

**Today.** No crate carries the set: none names `string_slice`,
`panic_in_result_fn`, `todo`, `unimplemented` or `cast_possible_truncation`.
In `kernel/`, `cargo clippy --target x86_64-unknown-none` with the set as
warnings reports 2,274 findings, 992 of them `arithmetic_side_effects`, 408
`indexing_slicing`, 394 `cast_possible_truncation` and 208 `disallowed_macros`.

**Constraints.**
- The set does not see a shift by a variable amount, `pow` or `abs`, though
  each panics under `overflow-checks`; nor a standard function it does not
  name, nor a panic inside a callee.
- `clippy::allow_attributes` checks outer attributes alone: an inner
  `#![allow(…, reason = "…")]` passes it and `allow_attributes_without_reason`.
- `disallowed_methods` and `disallowed_macros` read the nearest `clippy.toml`
  alone, and the root's reaches every crate beneath it, tier 4 included.
- The no-panic proof needs unwinding: its README says "The attribute is useless
  in code built with `panic = "abort"`". Both kernel targets print
  `"panic-strategy": "abort"` from `rustc -Z unstable-options --print
  target-spec-json`, so the proof has to come from a host build.
- `--clippy` lints no userland program, because the `toyos` toolchain ships no
  clippy (`src/clippy.rs:7-8`).

**Stages, in order.**
1. Tier 1, one area at a time. The first exit is one declaration of the set
   that every tier-1 crate reads in place of its own copy. An area's exit is
   its crate or kernel module forbidding the set under `cargo run -- --clippy`;
   `forbid` refuses any inner `#[allow]` or `#[expect]` of it, the `#[allow]`
   in `issues/design-debt/elf-domain-lint-line-not-yet-added.md`'s exit too.
2. The link-time proof on the parser entry points: works or not, measured. The
   exit is a step of `cargo run -- --ci host` whose host build refuses to link
   an entry point with a panicking path, or a `rejected` issue with the
   measurement.
3. The kernel, one module at a time. The stage ends when `kernel/src/main.rs`
   denies the set and forbids `clippy::allow_attributes` and
   `clippy::allow_attributes_without_reason` under `--clippy`, and a step of
   `--ci host` refuses an inner `#![allow]`: then every exception is an
   `#[expect(…, reason = "…")]`, and a bare `#[allow]` fails the gate.
4. The system services. The first exit is a userland shape in `src/clippy.rs`;
   a service's exit is its crate root under stage 3's attributes.
