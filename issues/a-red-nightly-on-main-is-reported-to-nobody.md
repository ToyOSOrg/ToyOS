---
status: open
kind: tooling
opened: 2026-10-08
---

# A red nightly on main is reported to nobody

`nightly.yml`'s scheduled run is a check of no pull request and no merge group:
its verdict blocks nothing, and no role's prompt reads it. Its `host` job, the
host cache's one writer, was red at its seal on 2026-10-05, 2026-10-06 and
2026-10-07 (runs 37292450697, 37444133685, 37601225884), after #722 brought
`gix` into the build system and took the cold tree from 7,605,844,073 B to
8,540,783,725 B, past the seal's 8,000,000,000 B. The last entry saved is run
37190643147's, of 2026-10-04. A review of another pull request found the red on
2026-10-07, three runs in.

Owner: the orchestrator's prompt (`.claude/agents/orchestrator.md`).

**Exit condition.** A red scheduled `nightly` on `main` reaches whoever lands
work before the next one runs, by a means a reader can name: a sentence in the
prompt that reads `gh run list --workflow nightly.yml --branch main --limit 1`
before a landing, or a required check that fails on it.
