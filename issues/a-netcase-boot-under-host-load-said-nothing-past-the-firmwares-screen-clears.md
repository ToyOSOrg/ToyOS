---
status: open
kind: defect
opened: 2026-10-09
---

# A netcase boot under host load said nothing past the firmware's screen clears

One whole `cargo test` on the development Mac, at the head of the branch that
added `https_fetch`, went red on `netstack_socket_churn` alone: `[qemu] Boot
timed out waiting for ===READY===; the console carried: nothing at all`, after
487 s. The boot's 16550 file holds 58 bytes, the firmware's
screen-clearing escapes (`ESC[2J ESC[01;01H ESC[=3h`, three times) and
nothing after them: no loader line. The same image booted green in the same
run for `https_fetch` and `libc_sockets`, and `netstack_socket_churn` alone a
minute later was green in 12 s.

The host's load averages as the suite began were 82.78, 94.69 and 92.31 on
14 cores, other worktrees' suites among them, and 36.29, 47.59 and 67.23 as
it ended; 37.01, 45.47 and 64.77 for the green run alone. The harness had
priced its liveness ceilings at 1.00x.

A second, at `216f33b35` on the same branch: `cargo test --test toyos-build
-- https_fetch` alone, beside this tree's own `cargo run -- --ci host` and
other worktrees' work, went red the same way after 64 s, its 16550 file the
same 58 bytes. Load averages 89.36, 87.83 and 82.56 as it began and 98.94,
91.36 and 84.48 as it ended.

Not reduced: whether the firmware is stuck or starved, which a QMP read of a
silent guest's registers would say (`tests/CLAUDE.md`'s method for a death
during boot), and at what rate.

**Exit**: a rate over repeated boots of this image beside other guests, each
silent one's registers read, that names where the firmware stopped; and the
fix at that cause.

**Owner**: the orchestrator.
