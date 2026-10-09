---
status: open
kind: tooling
opened: 2026-09-03
---

# The host job tracks whatever toolchain ships, and a runner roll reds every open pull request at once

`35383398^:.github/workflows/host-tests.yml`'s `host` job installs no Rust toolchain: it
runs `rustc -vV; cargo -V; rustup component add clippy` on whatever
ships that day, and no `rust-toolchain.toml` is tracked outside `rust/`.

Measured by the #382 review, same restored cache
(`host-macOS-ece3092cdf5b…`), same job, back to back:

```
w5b13-gop-mode  host tests run 33735573163  08:51Z  rustc 1.97.1 (8bab26f4f 2026-07-14)  success
w5b15-ready     host tests run 33735939932  08:55Z  rustc 1.98.0 (88d9e12ae 2026-08-18)  failure
```

Independently confirmed against both runs' own logs:

```
$ gh run view 33735573163 --json conclusion,createdAt,headBranch
{"conclusion":"success","createdAt":"2026-09-03T08:50:50Z","headBranch":"w5b13-gop-mode"}
rustc 1.97.1 (8bab26f4f 2026-07-14)   (run log, step "rust", 08:51:03Z)

$ gh run view 33735939932 --json conclusion,createdAt,headBranch
{"conclusion":"failure","createdAt":"2026-09-03T08:54:50Z","headBranch":"w5b15-ready"}
rustc 1.98.0 (88d9e12ae 2026-08-18)   (run log, step "rust", 08:55:54Z)
```

The runner's roll from 1.97.1 to 1.98.0 introduced clippy lints that fired on
files nobody touched — real findings under the new toolchain, not flakes:

```
error: using `chunks_exact` with a constant chunk size  (clippy::chunks_exact_to_as_chunks)
   --> toyos-fat32-check/src/dir.rs:172:26
error: using `chunks_exact` with a constant chunk size
   --> toyos-fat32-check/src/fat.rs:34:16
error: using `chunks_exact` with a constant chunk size
   --> toyos-desktop/src/input.rs:148:24
error: manual implementation of `midpoint` which can overflow  (clippy::manual_midpoint)
   --> toyos-mixer/src/channel.rs:25:18
error: manual implementation of `midpoint` which can overflow
   --> toyos-mixer/src/channel.rs:53:31
```

(pasted from `gh run view 33735939932 --log`). The fix for these five landed
alongside this record, plus three more sites the same lint hits under
`cargo clippy --workspace --all-targets --keep-going` that the failing run's
own `set -e` never reached before the script died on the first red pipeline —
`toyos-fat32/tests/common/mod.rs:563`, `tests/common/screen.rs:79`, and a sixth
site under the kernel's own two clippy invocations
(`kernel/src/loader/start.rs:146`), plus one unrelated new lint the kernel
arm alone surfaced, `clippy::map_or_identity`
(`kernel/src/syscall/dispatch.rs:278`). All were confirmed clean under
`RUSTUP_TOOLCHAIN=1.98.0 cargo run -- --clippy` and unchanged under the
default 1.97.1 after the fix.

## The decision this tree has not made for the compiler the way it made it for everything else that moves

A moving input under every verdict is a supply-chain decision, not a
convenience, and every guest lane's container image is pinned by digest, never
by tag; the host job's toolchain is not. The
`CLAUDE.md` principle for
`rust/`, this project's own compiler fork, is "kept current with upstream" — a
deliberate track-stable choice, stated and owned.

**The decision owed, not taken here:** pin a toolchain version in the host job
(`actions-rs`-style `rust-toolchain` input, or a root `rust-toolchain.toml`
covering the host workspace too) and roll it deliberately on its own PR when
the tree is ready to adopt a new compiler's lints — or keep tracking whatever
ships and accept that a runner-image roll reds every open pull
request until someone lands the fix, the way today's did. Both are legitimate
engineering positions; this entry does not choose between them.

## Whichever it is, its LLVM drops a loop's exit

Every host binary — `toyos-build`, the harness, every host test, every app's
host build — is compiled by an upstream rustc whose LLVM has the
ScalarEvolution fault the fork's no longer has (llvm/llvm-project#175729, open
upstream; `src/miscompile.rs` holds its reproducers for the fork's compilers
and for no host compiler). Measured for stable 1.99.0 on Apple silicon, whose
`caller` of `src/miscompile/last_exit.rs` over `u16` and over `u128` is a
branch to itself, and for 1.98.1 given the loop's shape by hand
(`issues/the-nightlys-macos-job-pins-rustc-1-98-1-for-a-hang-its-test-no-longer-shows.md`).
No version to pin is free of it, so a pin answers the lints above and not
this; it ends when a stable rustc compiles those reproducers right.
