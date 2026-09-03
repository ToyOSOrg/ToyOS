---
status: open
kind: defect
opened: 2026-09-03
---

# `discard_capture`'s refusal branch is a comment with no arm behind it, and the judge over it reads presence and not absence

Two holes left where `screen_survived_panic_not_blamed` closed the discard's
happy path (PR #382).

`kernel/src/drivers/panic_console/mod.rs:512-517` says a refused discard leaves
the latch owned so a survived panic can still be painted as the cause of death,
and that `CAPTURE_ACCESS` refuses only under a fatal reader. Nothing executes
that branch: reaching it needs a fatal reader live while a recovering CPU
discards, and no test in the tree arranges the two at once. It is the shape
w4-common names — a refusal-reason whose hazard nothing has fired.

The judge is the second hole. `screen_survived_panic_not_blamed` asserts the
panel **contains** `FATAL_HALT_NONCE`, never that the survived panic's own text
is **absent**. A panel carrying both — a composited paint, or a live tail whose
window still holds the first panic — passes it. The absence assertion is not
one line: `live_tail` renders the whole retained ring, which legitimately holds
the first panic's message on the green arm too, so distinguishing "painted from
the stale snapshot" from "painted live" needs a marker the snapshot cannot
carry rather than a text the ring may.

Site: `kernel/src/drivers/panic_console/mod.rs:512-523`, `kernel-loom/tests/panic_capture.rs`
(which models the latch and not the wiring), and `tests/toyos.rs`'s
`screen_survived_panic_not_blamed`.

Exit: an actuator holding a fatal reader across a recovering CPU's discard, and
a second marker written between the two deaths that the frozen snapshot cannot
hold, asserted absent from the panel.
