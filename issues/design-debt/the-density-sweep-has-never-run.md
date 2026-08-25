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

**The rule has a written home again, and a gate.** `src/prosegate.rs`'s module
header is where the three surviving kinds are now stated — the one-clause
invariant at the edit site, the boundary contract, and the refusal-reason at a
surprising decision, over a module doc that is the contract and nothing else —
and `src/prose-ledger` is the ratchet under it: one row per `.rs` file the tree
holds, naming the comment lines and the dated comment lines it is permitted, so
no file grows past what it carries today without a deliberate edit to that row.
This entry is no longer load-bearing for the rule, and the half of it that said
the rule had nowhere to live is closed.

**The ratchet is not the sweep.** Its seed is the tree exactly as it stands, so
every number below is what the gate now *permits* rather than a figure anyone
defends. It stops the density rising; it lowers nothing.

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

**The ledger's seed, 2026-08-25**, over every `.rs` file the tree holds outside
`rust/`: 670 files, **70,319 comment lines**, of which **306** carry a
`YYYY-MM` date. `kernel/src` seeds at 25,679 comment lines of 64,406 — the
2026-08-24 figure plus four days of landings.

**What is still owed is the sweep**: re-derive each over-dense file's comments
against `src/prosegate.rs`'s three kinds — narrating investigation and
measurement moves to the commit message, an invariant restates as the one
clause that is true rather than the paragraph that proved it, a date in a
source comment goes to the commit message or here — then re-record the swept
rows in `src/prose-ledger`, which the gate prints for exactly this purpose, and
lower its `DATED_TOTAL` to match. Re-run the measurement after to say whether
the density actually fell. None of that is designed here: a design that is
right is written as code, and this is not yet code.

**Promoted to `defect` 2026-08-25** (finding-lifecycle ruling). The owner asked
five times over one review and nothing had acted, and the measurement in this
file is the evidence that not acting had a cost: `kernel/src` went from 27% to
39.7% comment lines while the entry sat. Owed by whoever runs the sweep.
