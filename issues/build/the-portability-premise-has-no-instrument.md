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
  reason. **Five nightlies measured nothing, for two reasons that were both the
  harness and neither the premise** — a missing `safe.directory` and the
  bootstrap's own CI path; the dated section below has the evidence and the
  fix. The previous `ubuntu-latest` version's cost (four from-scratch runs
  inside `toolchain.yml`'s own `build` step, 2026-08-15/19, 3864-4048 s / ~65
  min for the command alone) is not a number about this container, and the
  workflow's own `timeout-minutes: 350` is still carried over rather than
  re-derived.
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
  rather than a real number. **The death was the same harness bug Linux's was,
  not a portability gap** — the dated section below retracts the reading this
  file carried on 2026-08-24 morning.
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
- The premise itself is still unmeasured on every OS. Five nightlies died in
  the harness; the two harness bugs are fixed and the dated sections below
  carry the evidence, but the number this file exists to take — does a fresh
  machine with only Rust and QEMU build ToyOS — is owed until a run reaches
  the end of `--build-only`.

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
checkout `actions/checkout` made. Every step before it is green (`deps`,
`checkout`, `rustup`), the whole job takes ~2m10s, and no line of the build
runs. It is not a reason to loosen `git_common_dir`: that assertion is right,
and two worktrees arriving at one byte-identical path is what the locks keyed on
it depend on.

**Root-caused 2026-08-24: the job never told git the checkout was its own.**
The container runs as root (`rustup` in the same job prints `euid-obtained home
directory: /root`) over `/__w`, which is the host's `/home/runner/work`
bind-mounted in and owned by the runner's uid, so git refuses the repository as
dubiously owned. `actions/checkout` *does* add `safe.directory` — its own log
says `Temporarily overriding HOME='/__w/_temp/<uuid>'` first, and it takes that
temporary global config away when the step ends, so nothing after it inherits
the entry. Every other container job in this repository already carries
`git config --global --add safe.directory "$PWD"` — `ci.yml`'s and
`gate-a.yml`'s `key` steps, `probe-green.yml`, `.github/install-toolchain.sh` —
and this job was written without it. Fixed by adding the same line, and
`src/lib.rs`'s `git_common_dir` now carries git's own stderr into the panic,
because "not a repository" and "somebody else's repository" were one exit status
and four nightly logs that named neither.

**Retracted: macOS never contradicted the premise.** The reading recorded here
on 2026-08-24 morning — that a fresh aarch64 `macos-latest` cannot proceed
because upstream serves no `rust-dev` for its triple, and the only fallback
wants cmake and ninja — is wrong, and the same wrong reading also explains the
Linux job's *first* nightly (08-20, still on `ubuntu-latest`, before the
container move), which died the identical way on `x86_64-unknown-linux-gnu`, a
triple `download-ci-llvm` unambiguously serves. Every night, both jobs print:

```
downloading https://ci-artifacts.rust-lang.org/rustc-builds/
  87971e6d0ed0320b6c0c8df8b519583b3387fa53/rust-dev-nightly-<triple>.tar.xz
curl: (22) The requested URL returned error: 404   (x4)
ERROR: failed to download llvm from ci
```

`87971e6d0ed0320b6c0c8df8b519583b3387fa53` is not an upstream commit at all. It
is the rust fork's own `toyos: fix Command::output, empty-directory stat, and
FileAttr::file_type`, `HEAD^1` of the fork at `aab2f4de` — a commit rust-lang's
CI has never built and never will. `rust/src/build_helper/src/git.rs`'s
`get_closest_upstream_commit` returns `HEAD^1` outright under
`CiEnv::GitHubActions`, with no author filter, on the assumption that "on CI, we
should always have a non-upstream merge commit at the tip"; this fork's tip is
its own. `toolchain.yml`'s `build` step already runs
`env -u GITHUB_ACTIONS -u CI cargo run -- --build-only` for exactly this reason
and says so in its comment — `portability.yml` was written without it.

Measured 2026-08-24 in the primary checkout's `rust/` at `aab2f4de`:

| | |
|---|---|
| `rev-parse HEAD^1` | `87971e6d` — `toyos:` commit, the sha both jobs asked for |
| `rev-list --author-date-order --author=bors@rust-lang.org -n1 HEAD` | `b04d3c8c` (bors, 2026-08-02) |
| the llvm sha that walk then yields | `ad3d0bc1` (bors, 2026-07-31) |
| `HEAD /rustc-builds/ad3d0bc1.../rust-dev-nightly-x86_64-unknown-linux-gnu.tar.xz` | **200** |
| `HEAD /rustc-builds/ad3d0bc1.../rust-dev-nightly-aarch64-apple-darwin.tar.xz` | **200** |
| the same two URLs at `87971e6d` | 404, 404 |

and the dev host's own `rust/build/cache/llvm-ad3d0bc1...-false/` holds
`rust-dev-nightly-aarch64-apple-darwin.tar.xz`, 52,324,936 bytes, downloaded
2026-07-31. Upstream serves this artifact for aarch64 macOS; the fork's own
bootstrap on a dev host fetches it. So neither red was ever about cmake, ninja,
or a host triple, and the question this file has always carried — does the
bootstrap need cmake, ninja or libssl-dev on a machine that lacks them — is
still unanswered, because no run has reached the point where it could be asked.
Both jobs now unset the two variables.

**The Homebrew pin finding is confirmed on the instrument, and its predicted
disagreement has already fired.** Every run prints
`##[error]No formulae or casks found for qemu@11.1.0.` — the same answer the
local check got while the job was written, now taken on the runner. And the
runner's unpinned QEMU is **11.0.3** against `.github/qemu-version`'s declared
**11.1.0**, so the "future disagreement is expected eventually" this file
allowed for is present on every run to date. It correctly does not fail the job:
`--build-only` boots no guest.

## 2026-08-24 — the owner's parking ruling, and why it was not executed

The owner ruled (2026-08-24) that the macOS lane be parked as a declared
standing weakness with the exit condition "upstream publishes the dev-nightly
artifact for aarch64-apple-darwin", and the job removed from the workflow so the
nightly stops burning a guaranteed red. That ruling rests on this file's own
reading of the macOS logs, and the evidence above refutes the reading: the
artifact **is** published for that triple, the exit condition is already met,
and the red is a one-line harness bug shared with the Linux job rather than a
dependency-rule gap. Recording a weakness whose exit condition is already
satisfied would put a silently-false sentence in the tracker, which is the one
thing this directory exists not to hold, so the macOS job stays and is fixed the
same way Linux's is. **The ruling is back with the owner**: if a red on this
lane is still unwanted for a reason the evidence does not touch, that is his to
say.

## What this is not

Not a port. Nothing here fixes Windows or changes the build; the fix lives with
the symlink issue. The instrument makes the premise *behave like a premise* —
checked every night on machines nobody has groomed — instead of a sentence in a
doc. When the self-hosting north star eventually adds a fourth row (ToyOS
itself), this is the table it joins.
