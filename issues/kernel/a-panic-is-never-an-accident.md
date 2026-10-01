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

Tiers 2 to 4 are programs, not crates: a tier holds every crate of this tree
its programs link, and a crate in two tiers is held to the stricter. The loader
is tier 2. The system services are init and every program one of the three
modes' configs starts at boot or marks `service = true`, as `build::shipped`
reads them: every boot start counts, `console` and diag's `toybox` too.

**The set**, all `clippy::`: `indexing_slicing`, `string_slice`,
`arithmetic_side_effects`, `unwrap_used`, `expect_used`, `panic`,
`unreachable`, `todo`, `unimplemented` and `panic_in_result_fn`; the lossy
casts `cast_possible_truncation`, `cast_possible_wrap`, `cast_sign_loss` and
`cast_precision_loss`, which for tiers 1 to 3 overrides their rejection in
`issues/build/clippy-stage-two-is-lints-one-at-a-time.md`; `disallowed_methods`
naming `slice::split_at`, `slice::split_at_mut`, `slice::copy_from_slice` and
`slice::clone_from_slice`; and `disallowed_macros` naming `core::assert`,
`core::assert_eq` and `core::assert_ne`. It does not see a shift by a variable
amount, `pow`, `abs`, a standard function it does not name or a callee's panic.

**Stages, in order.**
1. Tier 1, one crate at a time; an input boundary inside the kernel or the
   loader is moved into a crate of its own first. The first exit is one
   declaration of the set that every tier-1 crate's library is linted under in
   place of its own copy, its tests left at tier 4. An area's exit is its crate
   forbidding the set under `cargo run -- --clippy`.
2. The link-time proof on the parser entry points. The exit is a step of
   `cargo run -- --ci host` whose host build refuses to link an entry point
   with a panicking path, or a `rejected` issue with the measurement.
3. The kernel and the loader, one module at a time. The stage ends when
   `kernel/src/main.rs`, `bootloader/src/main.rs` and every crate of this tree
   `kernel/Cargo.toml` or `bootloader/Cargo.toml` links, unless stage 1
   already forbids the set in it, deny the set and forbid
   `clippy::allow_attributes` and `clippy::allow_attributes_without_reason`
   under `--clippy`, and a step of `--ci host` refuses an inner `#![allow]`
   and an `#[expect]` of the set over more than one finding.
4. The system services, once `issues/build/userland-programs-are-never-linted.md`
   has put userland in `src/clippy.rs`. The stage ends when every crate of this
   tree a service links by a normal edge, whatever its `cfg`, its own and
   `toyos` included, unless stage 1 already forbids the set in it, is linted
   by `--clippy` under stage 3's attributes and passes stage 3's step, and the
   step finds those crates itself.
