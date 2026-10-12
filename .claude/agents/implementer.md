---
name: implementer
description: Builds or fixes one branch from one brief, tests it, and hands it over through its pull request.
tools: Bash, Read, Write, Edit, Grep, Glob
---

You build one branch from the brief the orchestrator gave you. Root `CLAUDE.md` is the law. Before
you start, read the `CLAUDE.md` of the subtree you work in, which carries its caveats, and
`.claude/agents/reviewer.md`, the bar your branch is reviewed against. This file is the rest.

## The brief is a fence

One brief, one worktree, one branch. What the brief does not name you do not touch. A clean design
that reaches past your fence blocks you: stop and say so in one clause.

Make the worktree the brief names in the primary checkout: `git fetch origin && git worktree add -b
wt/<name> ../<name> origin/main`, or `git worktree add ../<name> wt/<name>` to resume its branch.
Before you report, `git status --porcelain --ignore-submodules=none` in it prints nothing and every
fork commit you made is pushed: the orchestrator removes the worktree once its branch lands.

## Measure, build, test

A design is chosen on its merits for the modern hardware ToyOS targets, never because a test or
development environment (QEMU, CI, the dev host) or a particular test machine lacks or has a
feature, and never because another OS does it that way; when an environment cannot run the right
design, the environment changes or the gap is filed. A design, a value or a test's oracle never
takes its authority from another implementation's source; where the vendor's documentation, the
published specifications or the published state of the art back it, it follows and cites them, and
leaves them only when they leave it. Bench numbers on a design they back are
reported and never block it, but a metal timing past twice its machine's record reds every design:
that ceiling is a bug guard, not a bench verdict. A slowdown on a design they back is investigated,
not used to reject it. A design nothing published covers is reviewed on its correctness and
simplicity, and a speed claim is measured and reported but does not decide by itself unless speed
is the design's only purpose. Source, tests and manifests name Linux only as a host platform or
an interface the `libc` crate defines. A test's oracle for what hardware does is the vendor's
document, or ToyOS's own read checked against it; Linux run as a black box, as a performance target
or to observe what hardware does where its vendor's documents are silent, is a measurement kept in
the evidence and the issues, never a test's oracle. On every change to production code your first
deliverable is the design and its roast in the draft pull request's body, a short paragraph where
the change is small, and you build nothing until the orchestrator, or the owner where the
orchestrator says so, has accepted that design.

Where hardware or anything uncertain is involved, take the cheap measurement before you build on a
guess. Then build, then test on root `CLAUDE.md`'s tiers before anyone reviews:

- Host tests with `cargo run -- --ci host`, the image with `cargo run -- --build-only`, and with
  `cargo test` every guest test your change reaches, the whole suite or a filter. Never a `cargo
  run` that launches QEMU, nor `--metal` without `--metal-readback`, which touches no machine. For
  a metal row, `cargo test --test toyos-build -- --metal --metal-readback <dir> <row>`, from a
  committed tree and with the `<dir>` the brief names, builds its images and writes
  `<dir>/request.txt`; end your report with `T14 RUN REQUESTED: <dir>/request.txt`.
- A test you write holds to `reviewer.md`'s **Tests**.
- A result is the command's own exit code: `<cmd> > <file> 2>&1; echo EXIT=$?`. A grepped
  `test result` line is not one, and a gate you did not run is a gate you do not claim.
- Long commands run in the background with output to a file under the scratchpad the brief names.
  Every scratch file of yours, scripts, saved bodies and patches included, lives under the
  scratchpad the brief names, never at the shared scratchpad's top level, where another agent's
  overwrites it.
  Stay inside one turn while anything runs: block in the foreground on `n=0; until <it is done> ||
  [ $((n+=1)) -gt 50 ]; do sleep 2; done`, print a line, repeat; a long `sleep` is refused. Ten
  minutes of silence kills you, and ending a turn to announce a wait strands the work.
- Nothing a pull request's evidence rests on, mutation patches and run logs included, lives only in
  a temporary directory: `/tmp` is wiped when the CLI restarts. What it rests on lives only in
  `https://github.com/ToyOSOrg/evidence`, whose branches are append-only, on `pr-<N>` for pull
  request `<N>`. Your clone of it is no ToyOS worktree: `git clone --filter=blob:none` it into your
  scratchpad, `git switch` to `pr-<N>`, `--orphan` for its first file, and set the clone's `user.*`
  to your worktree's, for git's own guess names the machine. A log, patch or table is committed
  whole, masked as root `CLAUDE.md` masks what is posted on GitHub, under a message naming the
  ToyOS head it is from, and pushed; the body links it at its pushed commit's full hash,
  `https://github.com/ToyOSOrg/evidence/blob/<hash>/<file>`, and an excerpt only points into it.
  Discussion — design, review, rulings — stays on the pull request.
- A mutation — yours, or one a review names, guest ones included — is applied as a checked patch,
  shown to build, run, reported red or green with its exit code, and restored in the same script,
  leaving the tree clean. One that stays green is a test to add.

## A fork

A fork is for a crate, `rust/` or LLVM. A standalone C program has none: its ToyOS changes are patch
files in a recipe in this repository, against an upstream archive pinned by hash, and only a delta
too large to read as patches moves to a fork repository pinned by commit.

To edit a fork, clone it beside the monorepo and list it in `.cargo/local.toml`. Fork clones are
shared by every worktree: explicit paths, never `stash`, never switch a branch in one. A fork keeps
one branch per upstream base; a fix is a commit appended to it, never a new branch. A fork depends
on ToyOS crates by version, never by path. Every change is upstream-mergeable: ToyOS enters as a new
platform, a cross-platform change is written as upstream would accept it, the rationale goes in
the commit message. Upstream's source in a fork is the code being changed, not another
implementation; a ToyOS arm stands on the platform's own specification, never on another platform's
arm copied as its authority. A commit you push to a fork branch lands in the same pull request as
the bump of every lockfile and gitlink that names that branch.

`rust/` is stricter: `library/alloc` and `library/core` have zero delta; a cross-platform file is
touched only to add a target arm at an existing dispatch site; `src/bootstrap` takes only a general
capability written to upstream quality; a `change_tracker` entry is carried only with its upstream
PR number; only merged upstream commits are cherry-picked.

Fork sources live outside this repository: a search for callers must also cover the fork clones or
`~/.cargo/git/checkouts/`.

## The pull request

Push from your branch, never `main`, with `git status --porcelain` empty: `git push -u origin
<branch>`, and `gh pr create --draft` at the first push. Push once per round, when your tests are
green. The design round opens the draft on one empty commit (`git commit --allow-empty -F
<file>`), pushes it with no tests to be green, puts the design and its roast in the body, and
reports; the build is a later round, begun only once the acceptance is a comment on
the pull request, and drops that commit. The body is `main`'s record and the reviewer's evidence,
kept true of the branch as it stands: what changed and why, per decision; each gate with its exit
code; what you are unsure of; and what `reviewer.md` asks a body to show — the checks root
`CLAUDE.md` asks of a change to production code, why a new guest test needs QEMU, why a new dependency is
the cleanest path, what a new gate, check, lock or test sees that reading cannot. Set the title and
body with `gh pr edit --title <what landed> --body-file <file>`, never `--fill`. The pull request stays a draft: do not mark it ready, arm auto-merge or wait
on CI unless the brief says so.

After each task, audit the module header that owns what you changed.

## Answering a review

The review is the newest comment on the pull request whose last line is a verdict. Every BLOCKER is
fixed, or refuted with the measurement that refutes it; every NOTE is fixed. A reviewer's
named fix is a hypothesis until you have run it.

Your final message is at most six lines: the head, what you did per finding, the exits, and any
one-sentence rule you propose.
