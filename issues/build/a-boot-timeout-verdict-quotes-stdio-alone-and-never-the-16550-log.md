---
status: open
kind: defect
opened: 2026-09-28
---

# A boot timeout's verdict quotes stdio alone, so an early kernel panic that only reached the 16550 log is invisible in it

`wait_for_ready` (`tests/common/qemu.rs`) reads the 16550 log (`uart_log`)
back only inside the `Err(RecvTimeoutError::Timeout)` arm's `!panic_aborts`
check — true only when the ready marker asked for is not `DEFAULT_READY`. A
boot waiting on the default marker never takes that arm, so when the boot
timeout fires (`start.elapsed() > boot_timeout`) its panic quotes only `seen`,
the lines collected from stdio, and never the 16550 file.

A guest that panics before virtio-console comes up writes that panic to the
16550 alone — the comment above the `Timeout` arm says so: "A guest that dies
before virtio-console init never reaches stdio at all; the UART file is the
only channel it has." So the one case that comment describes is exactly the
case the timeout's own panic message cannot see: the verdict says "Boot timed
out waiting for ===READY===; the console carried: nothing at all," and the
line that killed the boot sits unread in a file next to it.

Evidence: PR #572's control run at `87629411`. The 16550 log held `EARLY
PANIC: panicked at library/alloc/src/alloc.rs:659:9: memory allocation of
4096 bytes failed`, and the verdict said "Boot timed out waiting for
===READY===", quoting stdio alone; the panic line never appeared in it.

**Exit**: the timeout verdict always quotes the 16550 file's tail too, shown
by a test that stages an early panic. Owner: `tests/common/qemu.rs`, held by
the orchestrator.
