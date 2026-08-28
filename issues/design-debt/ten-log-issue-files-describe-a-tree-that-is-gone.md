---
status: open
kind: defect
opened: 2026-08-18
---

# `issues/` entries still describe the log subsystem the kernel no longer has

The log architecture landed on 2026-08-15: the kernel keeps a per-CPU record
ring and the console, `/bin/logd` owns `/log`, `kernel/src/log_file.rs` and
`kernel/src/drivers/log_ring.rs` are gone. The closing pass that was to retire
the entries that work answers never ran, so ten files described the byte ring,
the idle-loop flush and the kernel file sink as live. The table below is what
is left of that ten: a row goes when its entry is closed, so the count is the
table and never a number in this sentence.

Verified against the tree on 2026-08-18. Each row says what makes it closable;
none was closed here, because each needs its own reading of what durable rule it
carries before it is deleted (`issues/README.md`). A row leaves this table when
the entry it names is deleted, so what is listed is what is still owed rather
than what was found on the audit day.

Every file named below was **still present on 2026-08-24**, checked by
path and frontmatter and nothing more — no row's "what answers it" was re-argued
that day. The one row whose *blocking concern* had moved is
`redesign-the-log-subsystem`, and it is rewritten below.

| slug | area | what answers it |
|---|---|---|
| `redesign-the-log-subsystem` | design-debt | **Not a closing candidate, and the split it wanted has happened.** It is `kind: track`, `decided: 2026-08-19`, both halves approved as staged work — so it is open work rather than a stale description, and this pass leaves it alone. What it *does* carry is this entry's own defect: its six-row evidence table still lists `log.rs` (64 lines), `drivers/log_ring.rs` (549) and `log_file.rs` (564), none of which exist — `kernel/src/log/` is a directory of eight files — and its layout half prices `kernel/src` at 39 flat `.rs` beside seven directories, measured 2026-08-24 as 44 beside ten, three of them (`log/`, `object/`, `completion/`) already on its target list. Re-measuring that table is the track owner's, not a closing pass's |
