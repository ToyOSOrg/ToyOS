# ToyOS

An operating system built from scratch in Rust, held to a production-grade engineering bar — the bar is the changes, not yet the product. Modern x86-64 hardware (2020+), UEFI only; ARM64 planned — keep the architecture portable. The quality bar is shipping software: correct, efficient, minimal, zero silent debt. A tracked weakness is still a weakness: the honest answer about current state is "known, tracked, still true" — never "we have an issue for that."

## Where the rest of this lives

**This file is what every agent needs before it knows which subsystem it is in.** Detail lives below it and loads when you go there:

| | |
|---|---|
| `kernel/CLAUDE.md` | the caveats that bite kernel work |
| `userland/CLAUDE.md` | the server doctrine, and the caveats that bite userland work |
| `tests/CLAUDE.md` | the caveats that bite the harness |
| `src/CLAUDE.md` | boot modes, the locks, worktrees — the operational file |
| `issues/README.md` | the issue tracker: one file per issue, typed by kind; `ls` is the index |
| `.claude/agents/reviewer.md` | the review prompt the orchestrator spawns a reviewer with |

There are no spec documents. Rules live where they are enforced — a gate, a
module header, the redlist, the review prompt — and everything else is an
issue. Free text that merely describes the tree rots and is deleted, not
maintained. A `CLAUDE.md` never holds a list that a manifest, a directory or a
gate already answers; it points at that source instead.

A subdirectory `CLAUDE.md` loads when a file in that subtree is `Read`, and not from `Bash`. A rule whose violation is unrecoverable or invisible stays here; everything else lives where the work is.

## Principles

- **Zero legacy.** No backwards compatibility, no fallbacks, no workarounds, no BIOS, no 32-bit. Research state-of-the-art OS design instead of replicating older OSes.
- **Zero silent debt.** Dead code is deleted; every abstraction earns its place. A discovered compromise has exactly two legal outcomes: remove it, or record it with ownership, evidence and an exit condition — and it stays a present-state weakness until removed.
- **Fail fast, trust nothing.** Panics over silent degradation; exhaustive matches; the unimplemented dies loudly. Input that crossed a trust boundary is never trusted and never panics the kernel — it is refused.
- **The kernel never crashes from userland.** A kernel bug crashes loudly; a userland bug never reaches it.
- **No kernel threads.** The kernel creates no thread but the per-CPU idle loop: kernel work runs, bounded, on the thread or interrupt that caused it and is charged to it; long-running work with no owner is a userland server's.
- **Rust is first class.** Not POSIX, not C. Unrepresentable is best: prefer compile-time safety over runtime checks over tests.
- **Existing Rust just works.** A program that builds for other operating systems builds and runs on ToyOS unchanged; the ecosystem gains ToyOS support through forks carried upstream, never through ToyOS-specific replacement crates.
- **Development ergonomics above all.** Iteration speed beats feature count; tooling comes first.

## Architecture

> A snapshot, deliberately shallow — always read the code.

**Kernel** — minimal; new additions are discussed and justified. Resource management, scheduling, process lifecycle, filesystem, device arbitration. 2 MB pages, demand paging, PIE binaries, full SMP.

**Userspace daemons** — compositor, netd, soundd, sshd, logd. Each claims a device or capability from the kernel and serves its function; crash one and the kernel is fine.

**The log is a userland file.** `/system/bin/logd` reads records on a cursor and owns `/log`; the kernel keeps the record ring, the console and the panel, and writes no file. `SYS_FSYNC` reaches the device's cache flush because logd's durability claim rests on it.

**Syscall ABI** — `toyos-abi/`: struct layouts, syscall numbers, typed wrappers; completely unstable, read the code. Never add or change a syscall without discussion; a deleted syscall's number is retired, never reused. `toyos/` builds on it with typed handles, IPC framing, ports, namespaces and `surface` — userland uses `toyos`, the kernel uses `toyos-abi` only.

**Capabilities** — a process holds exactly what its parent moved into it, and among kernel objects there is nothing it can name to get more. No registry, no connect-by-name, no pid-as-authority: `/system/bin/init` builds every program's namespace and device claims from `system.toml` before spawning it, and a handle a process does not hold is a bug in that process — the kernel ends it rather than answering a word it can ignore. **Isolation is non-negotiable, and the filesystem is inside it**: a process names only the paths in the view its parent built for it, the unit of isolation is the program, and a user is the part of the tree a session was handed. Not yet true of files: the kernel still resolves every path against one machine-wide tree until the storage track's per-program views land.

**CPU state** — a CPU's control registers come from one declaration, applied by the BSP and by every AP and asserted on each; no read-modify-write decides what either holds.

**Firmware** — the kernel calls no UEFI service; every UEFI call ToyOS makes is the loader's, before ExitBootServices.

**Input** — the kernel delivers key *transitions*, never what one types; a surface turns one into the other. Translation, layouts, dead keys and escape sequences live in userland, one translator per surface.

**POSIX** — the kernel ABI and SDK are Rust-native and capability-shaped. POSIX lives in `userland/libc` (ours, not a fork) with explicitly relaxed rules. That layer may be ugly; the kernel may not.

## Dependencies

**Rust** and **QEMU** for development, on any host OS and architecture — the development machine is nothing special. Beside them, where no Rust tool does the job, only C or C++ tools ToyOS can one day build and run, declared in `check_prerequisites`. No binary for one host OS alone — a macOS binary is a hard no, and "only for tests" does not soften it; no Python, Perl or shell of our own; only general and widely used crates — one that does *our* job we write ourselves, and a driver crate never; third-party crates are used as published, and a fork carries a change written to upstream quality and goes when upstream has it. No upstream pull requests are sent for now: ToyOS needs more attention and more contributors before upstream projects take it seriously, and upstreams tend to refuse AI-first projects and their contributions. A third-party source ToyOS cannot build without changing it is carried as an unmodified-source packaging mirror with a byte-identity gate, not as a fork. The north star is **self-hosting**: nothing — build, test, or verification — rests on a host binary. Self-hosting means ToyOS rebuilds itself on ToyOS and reproduces the host's bytes; a bootstrap from source with no binary seed is out of scope.

Vendor firmware a device verifies by its maker's signature may be shipped: pinned by version and hash, redistributable unmodified, recorded in `NOTICE`, and loaded only by that device's own driver through its IOMMU domain; it never executes on the CPU.

The bar is not yet the tree. The standing failures are the two macOS FAT tools, macOS's `xcrun`, Go's `gh` and our own shell `diag/flash.sh`. `NOTICE` names every committed third-party file with its hash, upstream and licence; an image carrying `DOOM1.WAD` may not be sold.

- **toyos-ld** — frozen: everything links with rust-lld, and toyos-ld stays only as the linker inside ToyOS until lld runs there, then goes.
- **rust/** — Rust compiler/std fork with ToyOS platform support (submodule). Auto-bootstraps; kept current with upstream. Its rules: `.claude/agents/implementer.md`, "A fork".

## Build & test

The testing rules live where they are enforced: known reds in `src/redlist.rs`, tiers in `src/tiers.rs`, the PR gate and the nightly in `.github/workflows/`. Operationally:

- `cargo run` builds everything (toolchain, kernel, bootloader, userland, image) and launches QEMU; `--build-only` skips the launch. `cargo test` runs the QEMU harness; `cargo run -- --ci host` runs every host suite, as the PR gate's required `host` check does.
- **Agents never run QEMU.** An agent verifies with host tests and builds the image at most; the orchestrator runs every guest test, one suite at a time.
- **Both produce large output**: run them in the background and read the output file — `[N characters truncated]` means data was lost. A full boot is under a second; incremental builds finish in seconds.
- **Leave the machine as you found it.** The development machine is shared: every agent stops what it started, removes the worktrees and scratch build output it no longer needs, and never leaves an emulator, a build or a watcher running.

## Repository layout

The root `Cargo.toml`'s `[workspace]` `members` and `exclude` lists account for every crate in the tree, and `src/hostws.rs` reds on one in neither; every package they name says what it is in its `description`.

## Workflow

**One agent, one worktree, one branch.** `cargo run -- --worktree add <path>` makes one; never `git worktree add` by hand — the naive path clones the rust fork's history and takes the machine-global toolchain name from every other checkout. The primary checkout is not a workspace: it owns `rust/`, the rustup link and `main`; `cargo run -- --sync` moves it onto whatever GitHub merged.

- Stay on the current task. File what you find in `issues/` and do not go fix it; one file per issue, its README has the shape.
- If something blocks, stop and report it. Don't work around it.
- Never degrade audible or visual quality — even temporarily, even for a big win elsewhere — without the owner's explicit sign-off.
- **Never truncate command output.** No `| head`, `| tail`, `| grep` to reduce it; long output runs in the background and is read from the file.
- **Always be empirical.** Read actual output; run the code; investigate root causes instead of guessing.
- **Every written number comes from a command that was run.** An estimate or datasheet bound says so. Write commit messages with `git commit -F <file>`, never `-m` — a double-quoted `-m` substitutes backticks and the shell runs them.
- **Commit freely on your branch; land through a pull request.** `main` moves only through a merged PR. `gh pr create --draft` at the first push — CI runs on PRs and nothing else; `gh pr ready` plus a written `--title`/`--body-file` when finished (never `--fill`); `gh pr merge --auto --merge` enqueues on `main`'s required merge queue, which builds each merge's exact composition and runs the required checks on it before `main` moves; `cargo run -- --sync` after it lands. Never merge into `main` by hand. The PR's title and body become the merge commit's: write them as main's record. A modify/delete conflict is resolved by accounting for every hunk of the modified side, never by checking its headings survived. A merge that deletes a document also deletes every citation to it in the same merge, checked by searching the bare name as well as the path. An ABI change lands with the work that needs it: every worktree builds the toolchain its own sources name, so branches that change the ABI run side by side and none waits on another. Every merge leaves `main`'s tip compiling. A branch lands after a review against `.claude/agents/reviewer.md`, spawned by the orchestrator with its brief and judged by it.
- **Never rewrite history, and never touch `main`.** No `--amend`, no `rebase`, no `--force` — on your own branch as much as anywhere: a pushed hash may already be cited. `main` is protected — PR required, no force-push, no deletion, no bypass.
- **A red test is a defect unless `src/redlist.rs` disables it with its issue (`cargo run -- --known-red <test>`); a flaky test is disabled at once, never re-run.**
- **A high-risk change names its two checks.** Security boundaries, the scheduler, the ABI, filesystems, devices, memory management, concurrency primitives: the PR names the negative control or mutation that fails if the implementation is wrong, and one epistemically independent oracle — an external specification, a differential implementation, real hardware, a third-party checker, a formal model, or a recorded real failure. A second agent is not independence: five artifacts from one wrong model still agree. A mutation is a negative control only if it reverts the *whole* change onto the base the green arm was measured on — a one-line revert of a change that moved two things measures neither.
- **Timing and audio verdicts come only from metal.** A QEMU test asserts order, completion, content and counts, never how long something took, and plays no audio; its only clock is a hang ceiling.
- **Subagents wait in the foreground** — background notifications do not reliably re-wake them: explicit `timeout`s, and for longer work background once and block with a few long foreground waits, polling before each sleep.
- **An agent never waits on CI.** It arms auto-merge, reports, and exits. Sequencing across landings belongs to the orchestrator, done in passes on its own wake-ups; several finished branches land as one batch PR rather than as one agent babysitting N cycles.
- **Subagents get an explicit model, never the session default.** The orchestrator scopes, dispatches and verifies; it edits nothing. Match the tier to the judgment in the task: judgment-bearing coding gets a frontier model, mechanical execution from an exact brief a mid tier, and non-coding mechanical work the cheapest. Never encode a temporary usage circumstance as a rule.
- **An agent is spawned fresh for every task, never resumed into the next one.** A long transcript rots: a resumed agent carries the last task's assumptions, stale facts and a filled context into the new brief. Once it has reported it is done; a question back to it is fine, a new assignment is not.
- **Durable facts go in the module header at the site — never in private agent memory, and almost never in a `CLAUDE.md`.** A `CLAUDE.md` is pointers and caveats of the most general kind — it never cites an individual issue file, because it is not an issue tracker; **an agent edits one only when briefed to.** A rule that truly has no better home and whose violation is invisible or unrecoverable is *proposed as one sentence in the final report*, and the orchestrator declines it or briefs an agent to place it. The story of a change goes in its commit message; after each task, audit the module header that owns what you changed. A comment never restates a count that somebody else's landing moves.
- **Code is the product, not prose.** Prose exists only where it is load-bearing — where the code needs it to be read correctly. Stale or false prose is deleted, never corrected or rewritten, and the reviewer cuts every line that is not load-bearing.
- **Code is liability.** If code can be deleted, it is; code that does not earn its keep is deleted or simplified. Keeping code "just in case" or for want of knowing whether it is needed is not a reason — it is deleted. We are confident, and the reviewer enforces it.
- **A comment is one of three kinds or it goes** — the one-clause invariant at the edit site, the boundary contract, or the refusal-reason at a surprising decision, over a module doc that is the contract and nothing else. Chronology, measurements' provenance, past implementations, investigation stories and narration of the obvious live in commit messages and the tracker, never in source — a date in a source comment is the tell, and a `CLAUDE.md` carries none either. Moving a durable fact to the site means moving the invariant, never the investigation. `.claude/agents/reviewer.md` holds this; a `CLAUDE.md` never grows.

## Planned work

Staged work is an issue like everything else: `rg -l 'kind: track' issues/` lists every open track.
