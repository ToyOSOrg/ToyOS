---
status: open
kind: defect
opened: 2026-08-03
---

# `screen_early_panic`'s ready marker is published three steps before the screen it asserts on

Re-verified 2026-08-24 at `6091aa99`. The marker moved, the release point moved
*earlier*, and the race is the same one.

`ready_marker` for that boot is `EARLY PANIC:` (`tests/toyos.rs:3758`), and the
early branch of `#[panic_handler]` (`kernel/src/main.rs:171-192`) does, in this
order:

```
alert!("EARLY PANIC: {}", info);              // <- the harness stops waiting HERE
drivers::panic_console::capture();
unsafe { drivers::serial::panic_flush() };
drivers::panic_console::render();             // <- the pixels it then asserts on
cpu::halt();
```

**The `alert!` is what releases the harness, not the flush.** `PERCPU_READY` is
false in this branch, so `klogd` has not been spawned — it is started from
`kernel_main` immediately before the scheduler (`kernel/src/log/console.rs:105`)
— so `console::mode()` is `Drain::Inline` (`:90-96`, null `KLOGD` *is* that
mode), and `log::emit` drains to the backend synchronously on the producer's own
stack before it returns (`kernel/src/log/mod.rs:292-297`). The record is on the
wire at the first statement.

So the harness is released, takes its screendump, and may do both before
`render()` — a full-screen MMIO blit of an 8x16 text grid — has put a glyph
anywhere. The failure is `"EARLY PANIC:" not on screen` with a **completely
empty** decoded screen, which is that and not a rendering defect: a render that
ran and got the wrong glyphs would decode to something.

Measured under the old marker at HEAD `6abed71`, one session, on a host shared
with other agents: **2 failures in 7 runs** (one inside a full suite, one
isolated, five isolated passes). It is not the concurrent-build window
`issues/build/` describes — that one reports as a `panicked at src/build.rs` and
has no decoded screen at all — and it is not the guest dying, which `screendump`
reports separately.

The ordering itself is deliberate and should not move: the comment beside it
says the flush goes first so a fault inside the renderer "costs the screen and
never the serial report", which is the right trade on a machine with no
exception handlers yet. What is wrong is the *marker*: it names an event that
precedes the thing under test.

## The fix the tree has since grown, and the comment that hides the bug

`tests/common/qemu.rs:2497`'s `screendump_until` is the answer, and its own doc
comment states this defect as a general fact — "The panic handler's own path
paints after the drain that emits the report, so a marker on serial does not yet
prove a paint." `screen_panic_muted` (`tests/toyos.rs:3727`) and
`screen_late_panic` (`:3800`) both use it. `screen_early_panic` is the one panic
screen test still calling the one-shot `screendump()` (`:3762`).

**And it carries a comment asserting the opposite, which is why nobody has
fixed it.** `tests/toyos.rs:3746-3749` reads "render() runs before panic_flush,
so the marker reaching the UART proves the paint already finished — no sleep."
Both halves are false against `kernel/src/main.rs:189-190`: `panic_flush` runs
before `render()`, and the marker precedes them both. A closing pass that reads
the harness and not the kernel will believe it.
