---
status: open
kind: track
opened: 2026-08-08
decided: 2026-08-19
---

# Redesign the log subsystem, and re-shape `kernel/src`

**The owner decided on 2026-08-19: go.** Both halves are approved as planned
work. Sequencing, set by the orchestrator with the ruling: the log core waits
until pipeline 2's second pull request (the lock conversions behind
`issues/kernel/every-wait-in-this-kernel-is-a-spin.md`) has landed — the two
touch the same scheduler-adjacent paths and land one at a time. The directory
re-shape is a scheduling matter exactly as priced below: one clean pass in a
window with few worktrees in flight, and never interleaved with a code change.

The target shape stands as reviewed: a log core (ring + context stamping,
once) with serial, file and screen as independent sinks carrying explicit
backpressure — a slow sink drops-and-counts, never blocks, does no unbounded
work in a scheduler-adjacent path, and fails alone.

The original question, recorded verbatim because it was the owner asking:
*"should we redesign and rewrite the log subsystem and rethink if the current
file/folder structure of the kernel makes sense?"* (`kernel/src/log.rs`). What
follows is the evidence that made it decidable.

**The log subsystem, as it was when this was written.** Six places, no core:

| File | Lines | What it is |
|---|---|---|
| `log.rs` | 64 | the `log!` macro and a GS-validity flag |
| `drivers/log_ring.rs` | 549 | the ring, 64 KiB, and its drain policy |
| `log_file.rs` | 564 | the `/log` sink and its flush |
| `drivers/serial.rs` | 467 | the 16550 sink |
| `drivers/panic_console/mod.rs` | 1,188 | the screen sink, panic-only |
| `drivers/virtio_console.rs` | 221 | the second serial-shaped sink |

**Part of the core half has landed, so that table is a baseline and not a
description.** There is a `kernel/src/log/` core: eight files, with `mod.rs`
holding the ring, the per-CPU shards and the record type
(`kernel/src/log/mod.rs:15-30`), beside `console.rs`, `nested.rs`, `read.rs`,
`registry.rs`, `shard.rs`, `storm.rs` and `user.rs`. The three files the table
calls the log's own are gone: `kernel/src/log.rs`,
`kernel/src/drivers/log_ring.rs` and `kernel/src/log_file.rs` are none of them
in the tree. The file sink is not a kernel module at all — `/bin/logd` is an
ordinary user process that reads records on a cursor and owns `/log`
(`userland/logd/src/main.rs:1-10`). What has *not* landed is the rest of the
target shape: `kernel/src/drivers/serial.rs` (433 lines) and
`kernel/src/drivers/panic_console/mod.rs` (1,112) are still not independent
sinks carrying explicit backpressure, and the sins below are still patched
rather than designed away.

Its known sins were the argument for the question, and this records them as the
subsystem above had them: the flush was unbounded, uninterruptible and in front
of the scheduler pass, and userland `println!` shared the ring — both answered
by the architecture that landed on 2026-08-15, which removed the file sink; a
boot that wedged before the idle loop produced no output at all because the
ring's only drains were the timer tick and the idle loop (answered since by
`Drain::Inline`, gated by `pre_idle_wedge_speaks`); the ring's occupancy
is one of the pre-`hlt` recheck's conditions, so a CPU with bytes pending
declines to sleep; and `drain_serial`'s `BackendGuard::lock` spins with
interrupts disabled with no bound and no deadlock panic (both, CLAUDE.md's
idle-loop warning). Each has been
patched where it hurt. None has been fixed by a design.

The shape the review reached, **if** the owner wants it: a log core (ring +
context stamping, once) with serial, file and screen as independent sinks
carrying explicit backpressure — a slow sink drops-and-counts, never blocks,
does no unbounded work in a scheduler-adjacent path, and fails alone.

**The layout half, re-measured.** `kernel/src` is **50 flat `.rs` files**
(`ls kernel/src/*.rs | wc -l`) beside ten directories — `arch/`, `completion/`,
`drivers/`, `elf/`, `iommu/`, `loader/`, `log/`, `mm/`, `object/`, `sched/`.
The 39-beside-seven figure this entry opened with is three directories and
eleven files out of date; `log/` is one of the six the review named as the
target and it exists. `elf.rs` and
`loader.rs` became directories in `42b29c9`, which is the precedent. The flat
set mixes a filesystem adapter, an IPC primitive, two input devices, a page
cache, io_uring, the process table and two cfg-gated test actuators at one
level. The review's target was subsystem directories (fs/, ipc/, input/,
proc/, log/, time/), and the `syscall.rs` split already forces at least one.

Cost, so the question is priced: a directory move is `git mv` plus `mod` lines,
it touches no logic, and it collides with every worktree in flight — which is
why it is a scheduling decision and not a technical one.

Two smaller layout items ride the same answer. `usb_gate.rs` (242 lines) and
`input_merge_test.rs` (178) are correctly `#[cfg]`-gated at declaration and
call (`kernel/src/main.rs:34`, `:36`, `:408`, `:537`) and are never in an
ordinary build, but
they sit interleaved with production sources; the review's target is one
`kernel/src/gates/` directory so that what test machinery exists is auditable
in one listing. And `input_merge_test` is the tell that pure logic is trapped
in the kernel: the merge state machine (one held-set, one button-merge, both
bounded) is host-testable with synthetic multi-source streams, after which the
gate shrinks or goes.
