# ToyOS

A general-purpose operating system built from scratch in Rust, held to a production-grade engineering bar — the bar is the changes, not yet the product. Modern x86-64 hardware (2020+) and AArch64 under QEMU `virt`, UEFI only; keep the architecture portable. Its test machines decide its feature set, never its design. The quality bar is shipping software: correct, efficient, minimal. A tracked weakness is still a weakness: the honest answer about current state is "known, tracked, still true" — never "we have an issue for that."

## Where the rest of this lives

This file is what every agent needs before it knows which subsystem it is in. A subdirectory `CLAUDE.md` loads when a file in that subtree is `Read`, and not from `Bash`.

| | |
|---|---|
| `kernel/CLAUDE.md` | the caveats that bite kernel work |
| `userland/CLAUDE.md` | the server doctrine, and the caveats that bite userland work |
| `tests/CLAUDE.md` | the caveats that bite the harness |
| `src/CLAUDE.md` | the caveats that bite build-system work |
| `issues/README.md` | the issue tracker: one file per issue, typed by kind; `ls` is the index |
| `.claude/agents/` | each role's prompt; `reviewer.md` is the bar a branch lands against |

There are no spec documents. A rule a reader can check lives in an agent's prompt or at its site; code enforces only what reading cannot see — runtime behaviour, bytes, measurements — and everything else is an issue. A `CLAUDE.md` never holds a list that a manifest, a directory or a gate already answers; it points at that source instead.

## Principles

- **Zero legacy.** No backwards compatibility, no fallbacks, no workarounds, no BIOS, no 32-bit. Research state-of-the-art OS design instead of replicating older OSes.
- **Zero silent debt.** Dead code is deleted; every abstraction earns its place. A discovered compromise has exactly two legal outcomes: remove it, or record it with ownership, evidence and an exit condition — and it stays a present-state weakness until removed.
- **Code is liability.** If code can be deleted, it is; code that does not earn its keep is deleted or simplified, and not knowing whether it is needed is no reason to keep it. Tooling is code too: a gate, lock, check or test is added only for what reading cannot see.
- **Fail fast, trust nothing.** Panics over silent degradation; exhaustive matches; the unimplemented dies loudly; no defensive code. Input that crossed a trust boundary is never trusted and never panics the kernel — it is refused. Never a flat wait, in code or in a test: wait on the event, bounded by a timeout that fails loudly; a fixed delay only where a hardware document mandates it and offers no notification, cited at the site.
- **The kernel never crashes from userland.** A kernel bug crashes loudly; a userland bug never reaches it.
- **Userland first.** The kernel takes on only what userland cannot.
- **No kernel threads.** The kernel creates no thread but the per-CPU idle loop: kernel work runs, bounded, on the thread or interrupt that caused it and is charged to it; long-running work with no owner is a userland server's.
- **Rust is first class.** Not POSIX, not C. Unrepresentable is best: compile-time safety over runtime checks over tests, and nothing a type refuses is tested.
- **Existing Rust just works.** A program that builds for other operating systems builds and runs on ToyOS unchanged; the ecosystem gains ToyOS support through forks, never through ToyOS-specific replacement crates.
- **Apps are portable.** An app builds and runs on Linux under Wayland (never X11, which is legacy), macOS and Windows from the same source as on ToyOS; a host build that fails is fixed in the app or its dependencies, never by making the app ToyOS-only. A program is ToyOS-only only when its job exists only on ToyOS — it owns ToyOS devices or kernel objects, or manages ToyOS itself — and its manifest's `exempt` says which, and why (`src/userlandhost.rs`).
- **Development ergonomics above all.** Iteration speed beats feature count.

## Architecture

> A snapshot, deliberately shallow — always read the code.

**Kernel** — 2 MB pages, demand paging, PIE binaries, full SMP.

**Userspace servers** — `system.toml` names them. Each claims a device or capability from the kernel and serves its function; crash one and the kernel is fine.

**The log is a userland file.** `/system/bin/logkeeper` reads records on a cursor and owns `/log`; the kernel keeps the record ring, the console and the panel, and writes no file. `SYS_FSYNC` reaches the device's cache flush because logkeeper's durability claim rests on it.

**Syscall ABI** — `toyos-abi/`: struct layouts, syscall numbers, typed wrappers; completely unstable. The cleanest, most sustainable ABI beats convenience; a removed number is free. `toyos/` builds on it with typed handles, IPC framing, ports, namespaces and `surface` — userland uses `toyos`, the kernel uses `toyos-abi` only.

**Capabilities** — a process holds exactly what its parent moved into it, and among kernel objects there is nothing it can name to get more. No registry, no connect-by-name, no pid-as-authority: `/system/bin/supervisor` builds every program's namespace and device claims from `system.toml` before spawning it, and a handle a process does not hold is a bug in that process — the kernel ends it rather than answering a word it can ignore. **Isolation is non-negotiable, and the filesystem is inside it**: a process names only the paths in the view its parent built for it, the unit of isolation is the program, and a user is the part of the tree a session was handed. Not yet true of files: the kernel still resolves every path against one machine-wide tree until the storage track's per-program views land.

**CPU state** — a CPU's control registers come from one declaration, applied by the BSP and by every AP and asserted on each; no read-modify-write decides what either holds.

**Firmware** — the kernel calls no UEFI service; every UEFI call ToyOS makes is the loader's, before ExitBootServices.

**Input** — the kernel delivers key *transitions*, never what one types; a surface turns one into the other. Translation, layouts, dead keys and escape sequences live in userland, one translator per surface.

**POSIX** — the kernel ABI and SDK are Rust-native and capability-shaped. POSIX lives in `userland/libc` (ours, not a fork) with explicitly relaxed rules. That layer may be ugly; the kernel may not.

## Dependencies

**Rust** and **QEMU** for development, on any host OS and architecture — the development machine is nothing special. Beside them, where no Rust tool does the job, only C or C++ tools ToyOS can one day build and run (Python, Perl, CMake, make), each declared where `reviewer.md`'s Arrivals says. No binary for one host OS alone: a macOS binary is a hard no, and "only for tests" does not soften it. ToyOS's own code is Rust; it writes no Python, Perl or shell of its own. Only general and widely used crates, used as published — one that does *our* job we write ourselves, and a driver crate never — and a fork carries a change written to upstream quality and goes when upstream has it. Where security or another important reason asks, above all on a trust boundary or in the kernel, we write our own crate, with a clear boundary and well tested; the kernel takes as few community crates as it can justify and depends on no libc. No upstream pull request is sent for now. Third-party source is used unmodified, pinned by hash, wherever only its packaging has to change; a fork is for source changes only. The north star is **self-hosting**: ToyOS rebuilds itself on ToyOS and reproduces the host's bytes, and nothing — build, test, or verification — rests on a host binary; a bootstrap from source with no binary seed is out of scope. Ask of anything new: could this ever run inside ToyOS?

Vendor firmware a device or CPU verifies by its maker's signature may be shipped: pinned by version and hash, redistributable unmodified, recorded in `NOTICE`. A device's is loaded only by its own driver through its IOMMU domain and never executes on the CPU; CPU microcode is loaded by the kernel. `NOTICE` names every committed third-party file with its hash, upstream and licence.

- **rust/** — Rust compiler/std fork with ToyOS platform support (submodule). Auto-bootstraps; kept current with upstream. Its rules: `.claude/agents/implementer.md`, "A fork".

## Build & test

- `cargo run` builds everything (toolchain, kernel, bootloader, userland, image) and launches QEMU; `--build-only` skips the launch. `cargo test` runs the QEMU harness, and `cargo test --test toyos-build -- --metal` the T14's. `cargo run -- --ci host` runs every host suite: it is the `host` check a ready pull request and the merge queue run. The guest suite is their `guest / suite` check, under KVM; the nightly runs it under TCG.
- **A behaviour is tested on the cheapest tier that reaches it**: a type that makes the bug unrepresentable, then a host test, then a metal row on the T14, and a QEMU guest test last.
- **Agents run their own guest tests**, the whole suite or a filter, side by side. **The T14 is the orchestrator's alone: no other agent runs `--metal` without `--metal-readback`, which touches no machine.**
- **Timing and audio verdicts come only from metal.** A QEMU test asserts order, completion, content and counts, never how long something took, and plays no audio; its only clock is a hang ceiling.
- **A red test is a defect**: it is fixed, or deleted with its issue recording the commit that restores it; a flaky test is deleted at once, never re-run. A red seen only under load is no flake: it is a defect, recorded with the host's load.
- **A high-risk change names its checks.** Security boundaries, the scheduler, the ABI, filesystems, devices, memory management, concurrency primitives: a negative control where a defect would otherwise land unseen — the *whole* change reverted onto the base the green arm was measured on — and an independent oracle where one exists: an external specification, a differential implementation, real hardware, a third-party checker, a formal model, or a recorded real failure. A second agent is not independence.
- **Never truncate command output.** No `| head`, `| tail`, `| grep` to reduce it: long output runs in the background and is read from its file — `[N characters truncated]` means data was lost.
- **Leave the machine as you found it.** The development machine is shared: every agent stops what it started and never another's process, killing only by PID and waiting out a build that holds a lock its own needs (a key's in the host's store or its worktree's, `src/buildlock.rs`), and removes the scratch output it made once it no longer needs it.

## Repository layout

The root `Cargo.toml`'s `[workspace]` `members` and `exclude` lists account for every crate in the tree, and `src/hostws.rs` reds on one in neither; every package they name says what it is in its `description`.

## Working here

- **Stay on the task.** What you find off it is filed in `issues/`, one file per issue, and not fixed on the way. If something blocks, stop and report it; never work around it. What ToyOS cannot do yet is filed, and work on it starts only on the owner's go.
- **Never degrade audible or visual quality** — even temporarily, even for a big win elsewhere — without the owner's explicit sign-off.
- **Always be empirical.** Read actual output; run the code; investigate root causes instead of guessing. Every written number comes from a command that was run; an estimate or datasheet bound says so.
- **Never put the owner's email or any other personal data in a network request or its headers**; any `User-Agent` is `toyos-build (https://github.com/ToyOSOrg/ToyOS)`. Nothing that identifies his machines or network goes into the tree, a commit message or anything posted on GitHub, and one that has to be referred to there is named by where it stands and its kind, with no character of it; `src/sourcegate.rs` names the shapes.
- **One agent, one worktree, one branch.** The primary checkout owns `rust/` and `main`, and is no workspace; `.claude/agents/implementer.md` makes a worktree and `orchestrator.md` removes it. Never make one with `git clone`, and never run `git submodule` in one: either fetches the fork's history again, and `git submodule` writes `core.worktree` into the fork's shared config, which breaks git in the primary's `rust/`.
- **Commit freely on your branch.** `git commit -F <file>`, never `-m`. An agent does what it wants with the commits it made and has not pushed, in a fork clone too. A remote branch of this repository other than `main` that one implementer owns is that implementer's to amend, rebase and force-push; a fork's branches are append-only.
- **Never touch `main`.** It moves only through a merged pull request, by its required merge queue, and is protected — no push, force-push, deletion or bypass. A pull request's title and body become the merge commit's: write them as `main`'s record. A branch lands after a review against `.claude/agents/reviewer.md`. A modify/delete conflict is resolved by accounting for every hunk of the modified side, never by checking its headings survived. A merge that deletes a document also deletes every citation to it outside `issues/` in the same merge, found by searching the bare name as well as the path. An ABI change lands with the work that needs it: every worktree builds the toolchain its own sources name, so branches that change the ABI run side by side. Every merge leaves `main`'s tip compiling.

## Prose

- **Code is the product, not prose.** Prose exists only where it is load-bearing — where the code or the record needs it to be read correctly. Stale or false prose in source and docs is deleted, never corrected or rewritten; a record — a pull request's body, a prompt — is kept true of what it describes. A pull request edits no existing issue except the issue it takes up, or one a rule names as where it records something: any other stale or false issue is left as it is.
- **Durable facts go in the module header at the site** — never in private agent memory, and almost never in a `CLAUDE.md`. A `CLAUDE.md` never grows: it holds pointers and caveats of the most general kind, and never cites an individual issue file. The story of a change goes in its commit message. A comment never restates a count that somebody else's landing moves.
- **An agent edits a `CLAUDE.md` or a role prompt only when briefed to**, or to update a command, flag or step its own change removes or renames. A rule that truly has no better home and whose violation is invisible or unrecoverable is proposed as one sentence in the agent's final report.
- **A comment is one of three kinds or it goes** — the one-clause invariant at the edit site, the boundary contract, or the refusal-reason at a surprising decision, over a module doc that is the contract and nothing else. Chronology, measurements' provenance, past implementations, investigation stories and narration of the obvious live in commit messages and the tracker, never in source — a date in a source comment is the tell, and a `CLAUDE.md` carries none either.

## Planned work

Staged work is an issue like everything else: `rg -l 'kind: track' issues/` lists every open track.
