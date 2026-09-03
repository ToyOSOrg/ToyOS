---
status: open
kind: tooling
opened: 2026-09-02
---

# `--pr`'s sync calls a primary checkout that is on a branch "stranded"

`src/pr.rs`'s `sync` skips its "the primary is on another branch, so this
host's main was left where it is" report when `canonical(root) ==
canonical(&primary)` — the case where the checkout running `--pr` *is* the
primary. It then runs `git -C <primary> merge --ff-only origin/main` regardless
of which branch that checkout has out. On a primary checkout sitting on a
branch with commits of its own, git refuses the merge and `sync` answers with
`stranded()`: "this host's main carries commits origin/main has not got ... these
arrived from a landing that predates that", plus a `reset --hard origin/main`
suggestion. None of that is true — the local `main` is a plain ancestor of
`origin/main` and nothing is stranded.

Reproduced on 2026-09-02 while writing `the_branch_gets_main_before_it_is_pushed`:
a fresh clone on branch `wt`, `origin/main` moved one commit on, and `prepare`
returned that refusal rather than merging. The test now moves `main` itself
first (`git fetch origin main:main`) and the fixture passes, which is why this
is filed rather than worked around further.

CLAUDE.md says the primary checkout is not a workspace, so the shape is out of
contract — but the answer to an out-of-contract shape should be the sentence
`sync` already has for it, not a false report of lost commits.

**Fix.** Ask which branch the primary has out before the fast-forward, whether
or not it is this checkout, and give the same "left where it is" answer. The
guard that skips the question is the whole defect.
