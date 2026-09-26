---
status: open
kind: finding
opened: 2026-09-26
---

# A console holder's line state guards writers that no longer exist

Found in the review of PR #527. `kernel/src/drivers/serial.rs`'s
`ConsoleLine` keeps, per holder, a partial line, a `held` piece and the
protocol that queues a line longer than `MAX_CONSOLE_LINE` in pieces
(`continues`, and `kernel/src/log/console.rs`'s `MID_LINE`) so that no two
holders' output splices. On a machine with `/system/bin/logd` the only console
holder allowed to write is `logd` — a child given a console is refused its
write — and `logd` renders whole lines.

The state is then protection against a second writer the kernel no longer
admits. If `logd` handed the console whole lines of at most
`MAX_CONSOLE_LINE`, the pieces, `MID_LINE` and the per-holder buffer could go,
and `kernel/CLAUDE.md`'s "the object *is* the line buffer" with them — that
line is the owner's to move, and is why this is filed rather than done.

**Exit condition**: decided — either the state and the `CLAUDE.md` caveat go
together, with `console_line_atomicity` rewritten to judge the whole-line
contract, or this is folded into `ConsoleLine`'s doc as the reason it stays.
