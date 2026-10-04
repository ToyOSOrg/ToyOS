---
status: assigned
kind: defect
opened: 2026-09-28
---

# A syscall-panic reset the guest announced and QEMU never saw

**Owner: the panic path, held by the orchestrator.**

At `f9700b90`, `cargo test --test toyos-build -- --nightly syscall_panic_halts`
went red once (EXIT=1). The guest's own serial shows the panic, the reboot
countdown and its last line before reset:

```
panic: rebooting in 5 s unless a key is pressed, timed by the calibrated clock
panic: no key inside the bound, so nobody is here: returning this machine to firmware
```

— the guest announced that it was resetting. The harness reported:

```
syscall_panic_halts: QEMU never reported stopping: the kernel died inside a syscall and the machine carried on without its caller
```

QEMU's own `SHUTDOWN` event, the independent oracle `power::syscall_death_resets`
reads, never arrived inside the row's wait. The run took 99.8 s at a 2.96x
liveness width (fastest boot 3908 ms against a 1320 ms reference).

**The one mechanism proposed for this is measured and does not reproduce it.**
`issues/the-panic-lock-spin-limit-is-a-count-its-comment-calls-a-second.md`
held the panic path's console wire across the same reset (`wire-held-red.patch`,
`core::mem::forget(serial::try_wire())` before `acpi::reboot`) and got EXIT=0 at
`8e525da4` in 27 s, against 7–8 s for the fixed path at the same head — the held
wire costs about 19–20 s, not the ~95 s this red ran before giving up, and it
still reset inside budget. A held console wire is therefore not what happened
here, and nothing else measured explains it.

The panic path no longer waits on that wire at all (`reboot_now` now resets
through `acpi::reset_now`, not `acpi::reboot`), so this exact path is gone —
but the mechanism that produced the red is still unknown, and nothing rules
out another one reaching the same symptom: a guest that says it is resetting
and a QEMU that reports nothing.

**Evidence:** the log above, in full, at
`scratchpad/nokthread-r6/f9700b90-syscall_panic_halts-red.log` on
`wt/toyos-nokthread`.

**Exit condition:** either this reproduces again with enough captured around
it (a QMP trace, the guest's own reset timestamp, a second independent signal)
to name the real cause, or `syscall_panic_halts` runs clean often enough on the
reset-through-`reset_now` path that the risk is retired — recorded with the
run count that justifies it, not asserted.
