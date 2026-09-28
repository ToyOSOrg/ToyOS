---
status: open
kind: tooling
opened: 2026-09-28
---

# `GUEST_WEDGED`'s "not budget-scaled" contract is broken by its own pairing with `ceiling`

`ceiling_verdict`'s absolute backstop (`tests/common/qemu.rs:702`) is
`ceiling.max(GUEST_WEDGED)`, and `ceiling` is what every caller passes in
already `budget`/`budget_smp`-scaled by phase width and host speed — e.g.
`screendump_while_rendering`'s `budget_smp(timeout, self.smp)`
(`tests/common/qemu.rs:3314`). `GUEST_WEDGED`'s own doc
(`tests/common/qemu.rs:516-522`) says the backstop is "Not budget-scaled, and
that is the point of the pair … scaling it would only make that state cost an
hour at width 12." The `.max()` contradicts that claim directly: once a
scaled `ceiling` exceeds the unscaled 300 s `GUEST_WEDGED`, the backstop *is*
that scaled `ceiling`, not the 300 s the doc names.

Sighting: `netd_refused_accept` (base timeout 120 s, `tests/toyos.rs:10111`)
paid the scaled ceiling, not 300 s: `FAIL netd_refused_accept: timed out
after 2341s, with the guest still talking 8s ago`.

The code's other callers of `GUEST_WEDGED` all want the unscaled number: it
is used raw at `tests/common/update.rs:193` and `:264` and at
`tests/toyos.rs:3368`, and `qemu.rs:527-528` cites a measured 302 s wedge on
a shared boot, not a width-scaled one. `ceiling_verdict`'s `.max(ceiling)` is
the one place that number stops being 300 s.

Exit condition: the code and the doc agree — a wedged-but-talking guest is
ended at the unscaled `GUEST_WEDGED` backstop, matching every other caller —
or the doc at `tests/common/qemu.rs:516-522` is deleted if scaling the
backstop through `ceiling` is actually intended.

Owner: whoever next touches `ceiling_verdict` in `tests/common/qemu.rs`.
