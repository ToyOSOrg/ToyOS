---
status: open
kind: defect
opened: 2026-08-23
---

# Deleting an issue leaves its citations behind, and only `src/redlist.rs` notices

CLAUDE.md's law is that a merge which deletes a document deletes every citation
to it. Nothing checks it, and it has been broken eleven times.

Measured against `main` at `7ab9367b`, 2026-08-23. `git grep` over a tree
rather than `rg` over a checkout, for the reason in the last section:

```
git grep -hoI -E 'issues/[a-z-]+/[a-z0-9-]+\.md' <rev> -- . | sort -u \
  | while read -r p; do git cat-file -e "<rev>:$p" || echo "DANGLING: $p"; done
```

160 distinct paths, **11 of which resolve to nothing**. Two further hits,
`issues/area/bare.md` and `issues/area/staged.md`, are synthetic fixtures inside
`src/issuegate.rs` and are not citations — the command cannot tell them apart,
which is one thing a real gate would have to.

| dangling path | cited from |
|---|---|
| `issues/build/i8042-keyboard-pays-a-lost-sentinel-and-reds-the-durations-gate.md` | `issues/build/defect-events.md` |
| `issues/build/one-issue-file-carries-an-area-name-as-its-kind.md` | `issues/build/the-swarm-is-not-yet-falsifiable.md` |
| `issues/build/three-host-crates-are-tested-nowhere.md` | `src/hostws.rs` |
| `issues/design-debt/four-deletions-still-owed.md` | *its one citer has since been deleted with the work it tracked, so this row's dangling citation is gone — the count above is the measurement as it stood* |
| `issues/hardware/metal-sim-pointer-churn-red-again-on-main.md` | `.github/workflows/probe-green.yml` |
| `issues/isolation/shutdown-needs-no-capability.md` | `issues/kernel/the-capability-end-state-is-twelve-answers.md` |
| `issues/kernel/i8042-quarantine-health-line-count-is-vacuous.md` | `kernel/src/actuator.rs`, `tests/toyos.rs` |
| `issues/kernel/pagealloc-has-no-checked-window.md` | `kernel/src/arch/syscall.rs`, `kernel/src/arch/percpu.rs`, `issues/build/clippy-has-never-run-here.md` |
| `issues/kernel/scheduler-policy-behavior-has-no-quantified-suite.md` | `issues/build/the-swarm-is-not-yet-falsifiable.md` |
| `issues/kernel/two-i8042-verdicts-red-together-on-one-ci-shard.md` | `issues/build/defect-events.md` |
| `issues/kernel/user-pages-still-read-through-a-plain-deref.md` | `kernel/src/arch/idt/exceptions.rs` |

**Five of the eleven are cited from code or from a workflow, not from prose**,
which is the half that costs the most: a module header or a gate comment that
points at a write-up is making a claim a reader will try to follow, and a
pointer that misses reads as checked. `src/redlist.rs` says exactly that about
its own `source` field and is the only place that enforces it — `refusals()`
reads the file and refuses a row whose write-up does not resolve or does not
name the row's test.

**A slug is the identity, so the bare name is a second search and the command
above does not do it.** A citation written as
`` `compositor-and-netd-unbounded-accept` `` rather than as a path is invisible
to a path regex. Both had to be run by hand to close six entries on 2026-08-23;
a gate has to do both.

`src/issuegate.rs` is the natural home — it already reads every issue file on
every `cargo test --lib`, and its own module header names this as the adjacent
question it does not answer. What it would need is the other direction: the set
of paths and slugs the tree mentions, minus the set that exists.

## The instrument bit somebody once already

The first sweep of this used `rg -oI --no-heading -g '!.git'` and found **ten**.
`rg` skips dotfile directories unless it is given `--hidden`, so `.github/` was
never read and `probe-green.yml`'s dangling pointer was invisible — a
measurement that undercounted by one and looked complete. `git grep` over a
revision has neither problem: it reads exactly the tracked files, dotfiles
included, and it is pinned to a commit rather than to whatever a checkout
happens to hold. Any gate written for this reads the tree, not the working
directory.

Found while closing six issues on 2026-08-23; none of the eleven is one of
those six, whose citations were all deleted in the commits that deleted them.
