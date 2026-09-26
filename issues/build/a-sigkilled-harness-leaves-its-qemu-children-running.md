---
status: open
kind: tooling
opened: 2026-09-26
---

# A `SIGKILL`ed harness leaves its QEMU children running, and the next sweep unlinks their disk out from under them

`SIGKILL` of the harness process alone does not reach the `qemu-system-x86_64`
children it spawned (`tests/common/qemu.rs`): nothing puts them in the same
kill, so they keep running after their parent is gone. `toyos_tmpdir`'s sweep
then reclaims the killed run's directory — the very images those QEMU
processes still have open — and unlinks it while the guest is still alive,
holding the disk invisibly until the guest itself exits.

This is not a regression: the retired `src/scratch.rs` design (kept a killed
run's directory for 24 hours) had the identical gap for a killed run's QEMU
children, so #529 (which replaced that design) found it and correctly did not
block on it. It is unfixed either way and worth its own entry.

Whoever takes this picks one of the shapes the review named: a `pdeathsig` on
the QEMU child (Linux-only, so this needs a macOS-side answer too, or a
process-group kill sent alongside the harness's own `SIGKILL` handling), or a
harness-owned reaper that tracks every `Child` it spawned and kills the group
on the way out. The exit condition is a killed harness whose QEMU children are
gone (or at least detached from the reclaimed directory) by the time the next
sweep runs, without introducing a new hang if the child is unresponsive.
