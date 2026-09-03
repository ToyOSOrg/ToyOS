---
status: open
kind: tooling
opened: 2026-09-01
---

# Nothing distinguishes `panic_console::discard_capture` from a no-op

The narrowed half of what `#366` closed. It told `capture` from a no-op and left
this one where it was. `capture` is now read by
`screen_late_panic`: the panic path writes one record after the snapshot and the
test reads it on the console and not on the panel. `discard_capture`
(`kernel/src/drivers/panic_console/mod.rs:518`) has no such reading. Its arm is
the recovery branch (`kernel/src/main.rs:174`), which `test-late-panic` never
takes, and `screen_recoverable_untouched` compares two screendumps either side
of the recovered panic (`tests/toyos.rs:4765`, `before.identical_to(&after)`),
which is equal endpoints and not an untouched interval: a paint that is
repainted before the second dump passes it. Either way it holds whether or not
the discard did anything.

**The protocol is modelled; the wiring is not.** `kernel-loom/tests/panic_capture.rs`
covers what the latch owes — `a_recovered_panic_hands_the_snapshot_to_the_next_captor`
and `discard_cannot_admit_a_writer_under_a_fatal_reader` — so what is untested
is that the kernel function calls it, on the branch that should, and only there.
That is the same gap `capture` had and the same shape of instrument closes it:
a survived panic followed by a fatal one, with the panel asserted to name the
second and not the first.

Not a code defect. The function carries its own refusal-reason — a refused
discard leaves the latch owned so a survived panic can still be painted as the
cause of death — and that reason is why it must not be deleted on the grounds
that the suite is green.
