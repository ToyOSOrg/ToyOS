---
status: open
kind: tooling
opened: 2026-09-03
---

# The host job tracks whatever toolchain `macos-latest` ships, and a runner roll reds every open pull request at once

`.github/workflows/host-tests.yml`'s `host` job installs no Rust toolchain: it
runs `rustc -vV; cargo -V; rustup component add clippy` on whatever
`macos-latest` ships that day, and there is no root `rust-toolchain.toml` —
only `kernel/`, `bootloader/` and `userland/` pin one, each to a target list
and none to a version.

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
(`kernel/src/arch/syscall/dispatch.rs:278`). All were confirmed clean under
`RUSTUP_TOOLCHAIN=1.98.0 cargo run -- --clippy` and unchanged under the
default 1.97.1 after the fix.

## The decision this tree has not made for the compiler the way it made it for everything else that moves

`issues/build/two-container-images-one-unpinned-and-one-unconsumed.md` (still
open) already names the pattern: `route.yml:131` pins the T14's image to a
digest — "a
rebuild must not be able to change the QEMU or Rust a recorded number was taken
on" — specifically because a moving input under every verdict is a supply-chain
decision, not a convenience. The `CLAUDE.md` principle for `rust/`, this
project's own compiler fork, is "kept current with upstream" — a deliberate
track-stable choice, stated and owned. The host job's ambient `macos-latest`
toolchain has never been stated as either: it is not pinned like the T14 image,
and it is not declared track-stable like `rust/` — it simply moves when Apple's
runner image moves, silently, until a new lint reds every open pull request on
the same morning.

**The decision owed, not taken here:** pin a toolchain version in the host job
(`actions-rs`-style `rust-toolchain` input, or a root `rust-toolchain.toml`
covering the host workspace too) and roll it deliberately on its own PR when
the tree is ready to adopt a new compiler's lints — or keep tracking whatever
`macos-latest` ships and accept that a runner-image roll reds every open pull
request until someone lands the fix, the way today's did. Both are legitimate
engineering positions; this entry does not choose between them.
