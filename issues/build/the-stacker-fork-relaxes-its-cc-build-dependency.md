---
status: open
kind: tooling
opened: 2026-09-29
---

# The stacker fork relaxes its `cc` build-dependency from 1.2.33 to 1.2.0

`rust/Cargo.lock` pins `ToyOSOrg/stacker` branch `toyos` at `c25842ac`. The
branch sits on upstream `93c7abc` (0.1.23, released untagged), and besides the
ToyOS backend it changes `cc = "1.2.33"` to `cc = "1.2.0"` in both
`Cargo.toml` and `psm/Cargo.toml` — a hunk with no ToyOS content that no
upstream pull request could carry. Upstream raised the bound in `c5b0f27`
("Add support for Arm64EC").

The relaxation is what lets the fork resolve in the toolchain's workspace, by
inspection, not by reverting it: `rust/compiler/rustc_llvm/Cargo.toml` pins
`cc = "=1.2.16"`, the one `cc` `rust/Cargo.lock` holds. Upstream Rust locks
stacker 0.1.21 and psm 0.1.26 (`rust/Cargo.lock` at `b04d3c8c`, the fork's
merge-base with rust-lang/rust), and the `stacker-0.1.21` tag asks for
`cc = "1.1.22"`, which `=1.2.16` meets.

**Owner**: the toolchain fork's dependencies, `rust/Cargo.toml`'s
`[patch.crates-io]`.

**Exit**: the `toyos` branch carries no `cc` hunk — its ToyOS commits sit on
the stacker release `rust/Cargo.lock` resolves without one — and
`rust/Cargo.lock` is re-pinned onto it.
