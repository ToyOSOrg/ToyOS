---
status: open
kind: defect
opened: 2026-08-19
---

# "Rust + QEMU is everything you need" is the premise, and no instrument holds it on any OS but this one

The dependency rule says only Rust and QEMU. If that is true, then on a fresh
machine of any major OS, installing those two and running the build is the whole
setup. `.github/workflows/portability.yml` is now the instrument that checks
this nightly (`workflow_dispatch` too) — three jobs, Linux, macOS and Windows.
What each proves today:

- **Linux** (`linux` job) — moved off `ubuntu-latest` onto the same minimal
  `debian:sid` container `ci.yml`'s guest shards run in, closing the gap this
  file originally named: git, curl, ca-certificates, `build-essential`,
  python3 (`rust/x`'s own bootstrap dependency for a clean clone — never
  declared before because `ubuntu-latest` ships one already) and the declared
  QEMU (`.github/qemu-version`) apt-installed, `rustup` installed from
  `sh.rustup.rs`, then `cargo run -- --build-only` with no cache. A green run
  here now means the declared package list was the whole of what this needed,
  not that the hosted image's own tens-of-GB contents quietly covered the
  rest — the same move `ci.yml`'s guest shards already made, for the same
  reason. **Still not measured, and now for a reason of its own**: four of the
  five nightlies to date die in the job's own setup, before a line of the
  premise is exercised — see the dated section below. The previous
  `ubuntu-latest` version's cost (four from-scratch runs inside
  `toolchain.yml`'s own `build` step, 2026-08-15/19, 3864-4048 s / ~65 min for
  the command alone) is not a number about this container, and the workflow's
  own `timeout-minutes: 350` is still carried over rather than re-derived.
- **macOS** (`macos` job) — added. `host-tests.yml` already proved
  `macos-latest` reachable on this plan; this job points a `--build-only` at a
  fresh one for the first time. QEMU comes from Homebrew, and the workflow
  tries to pin it to `.github/qemu-version` first. **Finding**: the pin does
  not exist as a general mechanism — measured empirically against a macOS
  Homebrew install while writing this job (2026-08-20), `brew install
  qemu@11.1.0` answers "No available formula with the name \"qemu@11.1.0\"."
  (exit 1). Homebrew-core keeps exactly one version of the `qemu` formula —
  whichever it currently calls stable — with no versioned formula the way
  `python@3.11` or `openssl@3` have one, so a pin is only possible on the day
  the declared version happens to equal current stable. That same measurement
  found today's unpinned `qemu` *is* 11.1.0, so the job's first runs will
  likely show the declared and installed versions agreeing, but by
  construction and not by anything the workflow enforces — a future
  disagreement is expected eventually and is itself the finding this job
  exists to surface, not a reason to fail it (`src/main.rs`'s
  `check_prerequisites` only notes a QEMU version other than declared outside
  a guest-booting gate, and this job boots no guest). The "four macOS FAT
  tools" standing exception does not qualify this job's honesty the way this
  file previously worried: `--build-only` runs no test, and
  `src/image.rs`'s `format_fat32` builds both volumes this build writes with
  the pure-Rust `fatfs` crate — `newfs_msdos`/`hdiutil` are exercised only by
  `toyos-fat32`/`toyos-fat32-check`'s host test fixtures, in `host-tests.yml`,
  never in a `--build-only` path. It has run five times now and never got far
  enough to cost anything: it reaches the toolchain bootstrap and dies there in
  under five minutes, so `timeout-minutes: 350` still borrows Linux's ceiling
  rather than a real number. What it did establish is below, and it is the
  first thing on this page to actually contradict the premise.
- **Windows** (`windows` job) — unchanged. `cargo build -p toyos-build` only,
  `cargo run` being unreachable: `src/toolchain.rs`'s `link_host_target` calls
  `std::os::unix::fs::symlink` with no `#[cfg(unix)]` guard
  (`issues/build/the-build-system-does-not-compile-on-windows.md`), so the
  crate does not compile on this OS at all. This job is that issue's own
  standing measurement — it reds every night until the symlink issue closes,
  which is the declared frontier and the point, not a surprise. Its first
  green is that issue's proof of fix, and only then does `cargo run
  -- --build-only` belong in this job, matching Linux.

`nightly-red-portability` now reacts to either `linux` or `macos`, the same
"declared, not absorbed" reasoning as before: `windows` stays hand-excluded,
since its red is the tracked frontier this file already names and reporting it
nightly would be noise the redlist doctrine calls absorbed rather than
declared.

## What is still open

- The third step this file originally sketched — `cargo test` where
  virtualization allows, a TCG run where it does not — is still deliberately
  out of the first cut. `portability.yml`'s Linux and macOS jobs stop at
  `--build-only`; wiring in a guest boot means either job also becomes one
  `src/ci.rs`'s `every_gate_that_boots_a_guest_names_its_instrument` should
  reach (add the file to that test's `GATES` list and run
  `.github/instrument.sh`), which the current build-only jobs correctly do
  not do — neither calls `qemu::launch`.
- If a job here is ever expected to red on a real (not-yet-fixed) gap that
  is not already carried by its own filed issue the way Windows's is, the
  redlist doctrine's "declared, not absorbed" still needs a worked answer for
  a *workflow-level* red — today `nightly-red-portability` reacts to a
  `linux` or `macos` red and Windows is hand-excluded by a workflow comment,
  which does not generalise past one known case.
- Both jobs have now run, both are red, and neither red is the one their own
  comments predicted. The measurements and what they mean are the dated
  section below; the two things owed out of them are a Linux job that can
  reach the build at all, and an answer for the fork bootstrap's LLVM on a
  host `download-ci-llvm` does not serve.

## 2026-08-24 — five nightlies have run, and the instrument reads

`gh run list --workflow portability.yml` gives five scheduled runs, 2026-08-20
through 2026-08-24, **all five `failure`**, each 5-6 minutes wall clock.
`nightly-red-portability` concluded `success` on every one, so the reporting
half works. Per-job, from `gh run view <id> --log-failed`:

| night | linux | macos | windows |
|---|---|---|---|
| 08-20 | `failed to download llvm from ci` | same, plus the brew pin | E0433/E0599 |
| 08-21 | `is not inside a git repository` | same | same |
| 08-22 | `is not inside a git repository` | same | same |
| 08-23 | `is not inside a git repository` | same | same |
| 08-24 | `is not inside a git repository` | same | same |

**Windows is the one red this file already predicted** and it is unchanged:
`error[E0433]: cannot find 'unix' in 'os'` and `no method named 'as_raw_fd'`,
five errors, `could not compile 'toyos-build' (lib)` — exactly
`issues/build/the-build-system-does-not-compile-on-windows.md`, still the
declared frontier.

**Linux has never measured the premise: it dies in its own setup.** Since
08-21 the job reaches `Running target/debug/toyos-build --build-only` and then
panics at `src/lib.rs`'s `git_common_dir`:

```
thread 'main' (8423) panicked at src/lib.rs:63:5:
/__w/ToyOS/ToyOS is not inside a git repository
```

`git rev-parse --git-common-dir` fails in the `debian:sid` container against a
checkout `actions/checkout` made — the ownership refusal a container hits when
the workspace is not owned by the uid running git; the same job's `rustup` step
prints `$HOME differs from euid-obtained home directory` beside it. Every step
before it is green (`deps`, `checkout`, `rustup`), the whole job takes ~2m10s,
and no line of the build runs. So the "first real number" this file has been
waiting for is still not taken, and the reason is the harness rather than the
premise. **This is the first thing owed.** It is not a reason to loosen
`git_common_dir`: that assertion is right, and two worktrees arriving at one
byte-identical path is what the locks keyed on it depend on.

**macOS contradicts the premise, and this is the finding.** Every night:

```
downloading https://ci-artifacts.rust-lang.org/rustc-builds/
  87971e6d0ed0320b6c0c8df8b519583b3387fa53/rust-dev-nightly-aarch64-apple-darwin.tar.xz
curl: (56) The requested URL returned error: 404   (x4)
ERROR: failed to download llvm from ci
    HELP: 1) The host triple is not supported for `download-ci-llvm`.
          2) Old builds get deleted after a certain time.
Bootstrap failed while executing `build --stage 2 --warnings warn`
  llvm::Llvm { target: aarch64-apple-darwin }
```

then `src/toolchain.rs`'s panic: `the toolchain build failed and
rust/build/aarch64-apple-darwin/stage2/bin/rustc is not there.` So on a fresh
`macos-latest` (aarch64), Rust and QEMU are **not** everything you need: the
fork's bootstrap wants a prebuilt CI LLVM that does not exist for this host at
the pinned commit, and the only fallback is building LLVM locally — which wants
cmake and ninja, neither installed. That is precisely the question this file
recorded as never having been tested against a machine that lacks them, and the
answer is that the machine cannot proceed. **This is the second thing owed**,
and it is a real gap in the dependency claim, not a workflow bug: either the
bootstrap gets `download-ci-llvm = false` plus a declared cmake/ninja (which
widens the dependency set and needs the owner), or the fork's pinned commit has
to be one whose `rust-dev` artifact is served for `aarch64-apple-darwin`.

**The Homebrew pin finding is confirmed on the instrument, and its predicted
disagreement has already fired.** Every run prints
`##[error]No formulae or casks found for qemu@11.1.0.` — the same answer the
local check got while the job was written, now taken on the runner. And the
runner's unpinned QEMU is **11.0.3** against `.github/qemu-version`'s declared
**11.1.0**, so the "future disagreement is expected eventually" this file
allowed for is present on every run to date. It correctly does not fail the job:
`--build-only` boots no guest.

## What this is not

Not a port. Nothing here fixes Windows or changes the build; the fix lives with
the symlink issue. The instrument makes the premise *behave like a premise* —
checked every night on machines nobody has groomed — instead of a sentence in a
doc. When the self-hosting north star eventually adds a fourth row (ToyOS
itself), this is the table it joins.
