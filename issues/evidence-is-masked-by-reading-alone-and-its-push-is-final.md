---
status: assigned
kind: tooling
opened: 2026-10-12
---

# Evidence is masked by reading alone, and its push is final

`.claude/agents/implementer.md` sends every log, patch and table a pull
request's evidence rests on to `ToyOSOrg/evidence`, masked as root `CLAUDE.md`
masks what is posted on GitHub. Nothing reads such a file before it is pushed.
`build/sourcegate.rs`'s `no_tracked_file_identifies_a_machine_or_its_network`
reads the files this repository tracks, and the evidence repository's are not
among them. Masking there is the pusher's reading, and nothing else.

A value that reading misses cannot be taken back. A miss is as permanent as
one in a tracked file, which is the case the sourcegate gate exists for. The
values ToyOS itself prints that a T14 log carries are those of
`issues/toyos-records-carry-the-sticks-serial-the-cards-mac-and-the-resolvers-in-full.md`.

**Evidence.** `gh api repos/ToyOSOrg/evidence/rulesets/24915111` answers
enforcement `active`, rules `deletion` and `non_fast_forward` on `~ALL`, and
`bypass_actors: []`. While it stands, no pushed commit leaves its branch, and
the pull request links each file at that commit's hash. The repository is
public.

**Exit**: either `build/sourcegate.rs`'s `identifying` shapes read each file
before it is pushed to the evidence repository, or the owner rules that
reading is enough. Owner: the orchestrator, which holds it.
