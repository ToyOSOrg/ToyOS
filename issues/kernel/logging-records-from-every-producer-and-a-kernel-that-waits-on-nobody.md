---
status: open
kind: track
opened: 2026-09-26
---

# Logging: records from every producer, and a kernel that waits on nobody

What is built: every program init starts or launches writes records into a
log ring of its own that init makes and names to `logd` (`toyos/src/log/`);
writing never waits, allocates or makes a syscall, a full ring is a count
`logd` says, and soundd's mix thread writes a lane of its own wait-free. The
clock is a page (`toyos-abi/src/clock.rs`). `klogd` is the console's one
writer, with interrupts on, and a program's line reaches the console only
through `logd`, under its name. The kernel waits on `logd` neither at a
panic, which halts the other CPUs first and seals its tail in the black box,
nor at a stop, which init sequences through `logd`'s flush. `logd` syncs on an
alert or an interval, preallocates its parts, keeps every boot's first part,
and holds each program to an allowance. Severity is an ordered ladder.

What is left, in order:

**1. The kernel's ring, rebuilt as one lockless variable-length ring with a
global sequence** — the shape of Linux's `printk_ringbuffer`. The cursor becomes
one `u64`, so the CPU count leaves the ABI (`MAX_LOG_SHARDS` is `LogCursor`'s
width today); the k-way merge and the shard registry go; the loom models are
rewritten. The ring lives in persistent RAM, so a triple fault or a firmware
reset — which seal nothing today — leaves the last boot's tail, and the next
boot's `logd` reads it through `SYS_LOG_READ` under a flag and writes it to
`/log` rather than the loader copying a rendering of it. That is also what puts
a stopped boot's own last records — its stop record, its census, `Rebooting.` —
into `/log` rather than only the next loader pass's page. High-risk: a
loom-checked primitive and the ABI.
*Exit:* one ring, one sequence, a `u64` cursor, and a guest test that resets a
machine mid-boot finding the tail in the next boot's `/log`.

**2. Verdicts that match on fields instead of English.** A record carries its
severity and origin; the tests and metal verdicts this change touched match on
them. The rest still search sentences: about 600 string literals in `tests/`
and 33 sentence constants in `src/bootlog.rs`, where rewording a diagnostic
breaks a verdict. Records grow a key the way journald's `MESSAGE_ID` does, and
a verdict matches the key.
*Exit:* no verdict in `tests/` or `src/metal*.rs` reads a record by its prose.

**3. A machine whose `logd` ended says so.** Nothing restarts `logd` and
nothing names its end: init holds no `Process` handle to it, so the rings fill,
their writers go on unharmed, and the console says nothing.
*Exit:* init names `logd`'s exit on the console, and a guest test that ends
`logd` finds that line and every other daemon still serving.

**4. The control that stages a kernel writing a file.** `log-writes-the-file`
puts a kernel context back to appending records to `/log` through the VFS —
the coupling the design removed — and reds an I/O-depth measurement and an
audio-latency A/B, neither of which is taken today.
*Exit:* the control exists and both measurements red on it.

Constraints a reader would otherwise re-derive:

- A ring lives as long as the process init started with it: `logd` sweeps and
  retires it at that process's end, so a child the process spawned into its own
  slots and that outlives it writes into a ring nobody reads.
- A shared-memory region is a 2 MiB page, so a ring costs 2 MiB per program —
  what the pipe it replaced cost once written to.
- The console queue `klogd` drains is 64 lines; `logd` holds up to 1 MiB of
  program lines behind it and counts what does not fit.
- A raw `SYS_REBOOT` or `SYS_SHUTDOWN` loses whatever a ring holds unread: the
  one caller that has `logd` flush first is init's `power` port.
