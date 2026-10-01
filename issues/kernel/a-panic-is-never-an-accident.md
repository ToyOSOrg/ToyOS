---
status: open
kind: track
opened: 2026-10-01
---

# A panic is never an accident: the owner's rule, not yet enforced

| Tier | Rule |
|---|---|
| 1. Input boundaries | No panic at all: the set below is forbidden. Where the build allows, each parser's entry point also carries a link-time no-panic proof. |
| 2. The kernel | No implicit panic: the set is denied. A deliberate stop for a broken internal invariant stays, spelled out at its site as an `#[expect(…, reason = "…")]` whose reason names the invariant. |
| 3. System services | The same rule as the kernel. |
| 4. Apps, ports, test code and build tooling | Normal Rust. |

Tiers 2 to 4 are processes, not crates: a tier holds every crate of this tree
its processes link, and a crate in two tiers is held to the stricter. The
system services are init and every program `system.toml` starts at boot or
marks `service = true`, so tier 3 holds `toyos` and every crate of this tree
one of them links.

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
- `arithmetic_side_effects` reports an expression once, however many
  operators it nests: `x * x + y * y - 1` is one finding, so one `#[expect]`
  covers its four overflows.
- `disallowed_macros` naming `core::assert` fires on `const _: () = assert!(…)`
  and `const { assert!(…) }` too, which cannot panic at run time: `kernel/src`
  holds 41 and `toyos-transport` one. `clippy::panic` passes a `panic!` in a
  const item or block, so a compile-time check is `if !… { panic!(…) }` there.
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
   under `--clippy`, and a step of `--ci host` refuses an inner `#![allow]`
   and an `#[expect]` of the set over more than one finding. The step lints a
   copy in which each `#[expect]` of the set is a `#[deny]` whose reason is its
   own file and line: rustc attaches that reason to every finding the
   attribute governs, so one reason on two findings is one `#[expect]` over two
   sites. It also refuses a copy with fewer findings than `--force-warn` of the
   set, which reports those under an `#[expect]` too: the copy missed an
   `#[expect]`, as one inside a `cfg_attr`. Then every exception is an
   `#[expect(…, reason = "…")]` of its own, and a bare `#[allow]` fails the
   gate.
4. The system services, once `issues/build/userland-programs-are-never-linted.md`
   has put userland in `src/clippy.rs`. The stage ends when every crate of
   this tree a service links, its own and `toyos` included, unless stage 1
   already forbids the set in it, is linted by `--clippy` under stage 3's
   attributes and passes stage 3's step. The step finds those crates itself:
   it reads the services out of `system.toml` and `cargo metadata
   --format-version 1` over userland's workspace, walks the normal edges from
   each service's package whatever their `cfg`, as `src/licence.rs` walks them
   from what ships, and keeps every package whose manifest lies in this
   repository. A registry or git package is third-party and is held by
   `CLAUDE.md`'s "Dependencies", not by this track.
