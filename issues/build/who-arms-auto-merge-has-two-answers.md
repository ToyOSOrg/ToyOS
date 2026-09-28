---
status: open
kind: tooling
opened: 2026-09-28
---

# Who arms auto-merge has two answers

Root `CLAUDE.md` says an agent "arms auto-merge, reports, and exits".
`.claude/agents/implementer.md` says "Do not arm auto-merge … unless the brief
says so", and `.claude/agents/orchestrator.md`'s Land section has the
orchestrator run `gh pr merge --auto --merge`. An agent reading both follows
whichever it read last.

Exit: one owner of arming auto-merge, named the same in every file that says it.
