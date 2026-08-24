---
status: open
kind: defect
opened: 2026-08-24
---

# The density sweep has never run

Split out of `issues/design-debt/the-comment-density-position.md`, whose
`kind: rejected` contradicted a body that said work was owed — the tracker's
own law (`issues/README.md`) says the *kind* is wrong when that happens. The
hardware-name half of that file is executed (see the PR that filed this one);
this is what remains: the comment-density half, which nothing has acted on.

**The owner's complaint, five times over one review**: *"the whole codebase
has too many comments. good code speaks for itself. accompanied by spec
documents per subsystem or whatever that should suffice"* (`main.rs`), *"why
so long comments?"* (`bootloader/main.rs:164`), *"does it make sense to have
so many comments in each source file or should we instead refer to the spec
in the module and just let the code speak for itself?"* (`sched/driver.rs`),
*"theres slop narration in the comments"* (`log_file.rs`, since deleted), and
*"'now runs the whole way' thats narration slop"* (`bcachefs_adapter.rs:17`).

**The rule this would measure against no longer has a written home.** As of
2026-08-08 it was CLAUDE.md's "no slop comments" paragraph, narrowed by the
2026-08 code-quality review (`specs/code-quality-review-2026-08.md` at the
time, `c6192619`) to three surviving kinds: the one-clause invariant at the
edit site, the boundary contract, and the refusal-reason at a surprising
decision — over a module doc that is the contract and nothing else, target
ten lines. Two later, unrelated owner rulings removed the text itself: the
root file's byte-budget trim (`6ce687dd`, 2026-08-13) cut CLAUDE.md from
38,284 bytes to 12,017 and the paragraph did not survive, and the spec
corpus that might have carried it forward was deleted whole (`c7efcd30`,
2026-08-19, "documentation carries no gates"). The three-kinds rule is
recorded here because nowhere else in the tree states it; a sweep that
wants to gate on it has to decide where it lives first — a module header
convention, most likely, since that is where this tracker's own norms put a
durable rule now.

**Re-measured 2026-08-24** (lines whose first non-space characters are `//`,
`/*` or `*`; trailing comments are not counted, so the real figure is higher
— same method as the 2026-08-08 measurement, not comparable file-for-file
since the hardware-name scrub landed in the same PR that filed this entry
and touched comment text, not comment count):

- `kernel/src`: **25,424 comment lines of 64,045 — 39.7%** (was 27% on
  2026-08-08, 11,920/43,739 — the tree grew 46% and its comment share grew
  faster than its line count).
- First-party Rust as a whole (`kernel bootloader toyos toyos-abi userland
  toyos-desktop toyos-elf toyos-sched src`): **45,151 of 148,615 — 30.4%**
  (was 22.1%, 21,424/96,848).
- Worst files over 200 lines: `actuator.rs` 701/1002 (**70%**), `heartbeat.rs`
  177/272 (65%), `writeback.rs` 170/283 (60%), `arch/mod.rs` 124/205 (60%),
  `block.rs` 290/492 (59%), `arch/idt/nmi.rs` 121/208 (58%). `log_file.rs`,
  the worst file in the 2026-08-08 list at 66%, is deleted (`kernel/CLAUDE.md`:
  "nothing on the idle path touches a filesystem" — it was the last one).

**What does not exist is the sweep, and nothing in the tree measures density
or would notice it rising** — both true today exactly as they were
2026-08-08, and the density grew regardless. A sweep would: pick a written
home for the three-kinds rule (most likely a workflow-file convention, since
CLAUDE.md itself now carries almost no subsystem-specific prose), re-derive
each over-dense file's comments against it — narrating investigation and
measurement moves to the commit message per the (now unwritten but still
practiced) slop rule, an invariant restates as the one clause that is true
rather than the paragraph that proved it — and re-run this measurement after
to say whether the density actually fell. None of that is designed here: a
design that is right is written as code, and this is not yet code.

**Promoted to `defect` 2026-08-25** (finding-lifecycle ruling). The owner asked
five times over one review and nothing has acted, and the measurement in this
file is the evidence that not acting has a cost: `kernel/src` went from 27% to
39.7% comment lines while the entry sat. Owed by whoever runs the sweep, whose
first step is giving the three-kinds rule a written home — it is recorded
nowhere else in the tree, so this file is currently load-bearing for it.
