---
status: owner
kind: question
opened: 2026-09-06
---

# `CLAUDE.md` and the agent prompts disagree on who lands a branch

`.claude/agents/` now holds the landing protocol and root `CLAUDE.md`'s
Workflow bullets still hold an older one.

- `CLAUDE.md:118` — *"`gh pr ready` plus a written `--title`/`--body-file` when
  finished"*, addressed to the working agent. `.claude/agents/orchestrator.md`
  takes `gh pr ready` for the orchestrator alone.
- `CLAUDE.md:124` — *"It arms auto-merge, reports, and exits."*
  `.claude/agents/implementer.md` forbids the implementer auto-merge outright;
  only the orchestrator arms it.

Which declaration is the law: either the two clauses move to the agent files
that now own them, or the agent files are wrong and say so.
