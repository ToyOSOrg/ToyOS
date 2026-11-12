---
status: open
kind: tooling
opened: 2026-11-12
---

# A fork registered in `.cargo/local.toml` dirties the nested lockfiles, and the build then stops

`.cargo/config.toml` includes the untracked `.cargo/local.toml`. That is where
an implementer registers a fork clone under edit, as a `[patch]` of its git
source and of crates-io (`.claude/agents/implementer.md`, "A fork"). Cargo
reads that configuration for every invocation under the checkout, including
those in workspaces the patch has nothing to do with: `rust/library`,
`rust/src/bootstrap` and `sdk/libc`. Each of those resolves without the
patched crate and writes `[[patch.unused]]` entries for it into its own
`Cargo.lock`. `build/llvm.rs`'s `refuse_uncommitted_bootstrap` then finds
`rust/src/bootstrap` holding changes no commit does, and the build panics
there. So a checkout with a fork registered as the instructions say builds no
image and no sysroot until the registration is removed and the nested
lockfiles are restored. Meanwhile `git status` shows `rust` and
`sdk/libc/Cargo.lock` modified.

Seen at `3c5c55584` with the mio fork registered:
- a sysroot build wrote the entries into all three lockfiles;
- `cargo run -- --build-only` then panicked at `build/llvm.rs:329`.

Reproduced at `f9e3135d2` without a sysroot build:
- `cargo metadata` in `rust/src/bootstrap` and in `sdk/libc` writes the entries;
- `cargo run -- --build-only` exits 101 at the same assertion
  (https://github.com/ToyOSOrg/evidence/tree/a3ad7b166e2d80021bf2d633adcd704096c66f17/pr-1276/local-toml).

Building one crate with the patch given as `cargo --config` leaves every other
lockfile alone.

**Exit**: a fork registered as the instructions say reaches the builds that
consume it, and no lockfile outside those workspaces changes. Alternatively,
the instructions name a registration that has that property.

**Owner**: the orchestrator, for the build system and the implementer's fork
rules.
