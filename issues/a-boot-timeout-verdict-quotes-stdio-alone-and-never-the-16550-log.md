---
status: assigned
kind: tooling
opened: 2026-09-28
---

# A boot timeout's verdict quotes stdio alone, so an early kernel panic that only reached the 16550 log is invisible in it

Evidence: PR #572's control run at `87629411`. The 16550 log held `EARLY
PANIC: panicked at library/alloc/src/alloc.rs:659:9: memory allocation of
4096 bytes failed`, and the verdict said "Boot timed out waiting for
===READY===", quoting stdio alone; the panic line never appeared in it.

**Exit**: the timeout verdict quotes the 16550 file's tail the same way the
`Disconnected` arm already does — `fs::read_to_string(uart_log)`
(`tests/common/qemu.rs:5147`) — rather than adding a second reader, shown by
a test that stages an early panic. Owner: `tests/common/qemu.rs`, held by
the orchestrator.
