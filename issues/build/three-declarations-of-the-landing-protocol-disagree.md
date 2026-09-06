---
status: owner
kind: question
opened: 2026-09-06
---

# Three declarations of the landing protocol disagree

`.claude/agents/` now holds the protocol, and root `CLAUDE.md`'s Workflow
bullets still hold an older one. Only the owner or the orchestrator may edit a
`CLAUDE.md`, so this is recorded rather than fixed.

- `CLAUDE.md:118` — *"A branch lands after a review ... spawned by the
  orchestrator with its brief and judged by it"*. The reviewer's file says the
  orchestrator is the judge and reads no finding, so "judged by it" is true of
  the branch and false of the review.
- `CLAUDE.md:124` — *"An agent never waits on CI. It arms auto-merge, reports,
  and exits."* `.claude/agents/implementer.md` forbids the implementer
  auto-merge outright; only the orchestrator arms it.
- `CLAUDE.md:118` — *"`gh pr ready` plus a written `--title`/`--body-file` when
  finished"*, addressed to the working agent.
  `.claude/agents/orchestrator.md` takes `gh pr ready` for the orchestrator
  alone.

The question is which declaration is the law. Either the three Workflow clauses
move to the agent files that now own them, or the agent files are wrong and say
so.
