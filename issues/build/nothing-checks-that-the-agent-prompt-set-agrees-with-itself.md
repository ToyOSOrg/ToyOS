---
status: open
kind: tooling
opened: 2026-09-06
---

# Nothing checks that the agent prompt set agrees with itself

Three facts about `.claude/agents/` have to agree and nothing reads any of
them: the files on disk, the `!` negations in `.gitignore` that admit them, and
the rows in root `CLAUDE.md`'s table that point at them.
`rg -n 'claude/agents' src/ .github/ --hidden` returns nothing, exit 1.

`.gitignore` is deny-by-default under `.claude/`, which is the intended shape
and also the failure mode: a fourth prompt file, or a negation with a typo in
it, is untracked and invisible — no gate reds, and `git status` does not list
what it ignores. A table row pointing at a file nobody admitted is a dead
pointer of exactly the kind `issues/README.md` refuses elsewhere.

Exit condition: one host test that reds when `ls .claude/agents/*.md`,
`.gitignore`'s negations under that directory, and `CLAUDE.md`'s table rows are
not the same set.

Weigh it against `issues/build/the-tooling-is-a-review-prompt-and-three-workflows.md`
first, which moves rules out of gates and into the prompt: this asks for a gate
in the direction that track is emptying. What the track does not answer is who
notices a prompt file that was never committed.
