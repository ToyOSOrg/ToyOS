---
status: open
kind: defect
opened: 2026-09-26
---

# A zero-byte pipe write wakes the reader's watch

`sys_write_nonblock` (`kernel/src/arch/syscall/io.rs`) wakes the pipe's
readers after every write that returns `Ok(n)`, `n == 0` included, and
`complete_pending_for_event` (`kernel/src/inbox.rs`) completes a pending
`READABLE` watch on that wake as ready without looking at the ring. So a
zero-byte write, which moves nothing, completes the reader's watch as
readable while the reader's `read` still answers `WouldBlock`.

netd's liveness probes are exactly such writes, once a pass, into every
piped connection's receive pipe and every piped listener's notify pipe, and
netd passes every millisecond while a piped connection lives. Every client
watching one of those pipes is woken about a thousand times a second for
nothing. Seen, not guessed: a guest waiting on its receive pipe with a
`READABLE` watch was completed as ready within `netd_refused_pipes`' first
two seconds and then read `WouldBlock` (the run with netd's send-side
refusal mutated away, before that test learned to re-check after a wake).

Exit condition: a write that moves no bytes wakes nobody, or a wake
completes a watch only if its direction is ready, with a test whose reader
watch stays pending across a zero-byte write.
