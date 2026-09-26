---
status: open
kind: defect
opened: 2026-09-26
---

# `swap <service>` without a length cannot tell a cut input from the binary's end

`/system/bin/swap` has two forms. `swap <service> <sha256> <length>` reads
exactly that many bytes and holds them to the caller's digest, so an input cut
short is refused before init hears of it. `swap netd < netd` — the form an owner
types — reads to the end of its input and computes the digest itself, so the
digest is of whatever arrived: a connection that ends mid-upload closes the
program's input exactly as the end of the file does, and init installs the
bytes that made it.

What stands behind it today is init's probation (`toyos_swap::PROBATION_MS`):
a truncated ELF that does not spawn, or dies inside the window, is `failed`
and the binary it replaced runs again. A truncated binary that spawns and runs
for five seconds is in service.

**Exit**: the short form carries its length in the stream it already reads —
or sshd tells a program whose connection died before its input ended by a
signal distinct from EOF — and a guest test cuts an upload mid-way and asserts
the swap is refused before init is asked.
