---
status: open
kind: defect
opened: 2026-09-13
---

# A deliberate wedge inside a driver panics every other CPU that wants its lock

`deadline::stage_a_wedge` stops every CPU at its next scheduler pass. The two
arms that use it from the shutdown syscall (`wedge-before-reset`,
`hard-lockup-probe`) hold no lock when they stop, so nothing else in the machine
is waiting on them. An arm that stops *inside a driver* does: the USB wedge arms
(`usb-wedge-data-owed` and its two siblings) stop with the xHCI controller lock
and the block layer's partition lock held for the rest of the boot.

`sync::Lock`'s deadlock detector then panics whichever CPU next reaches the
disk. On the T14 that is `logd`'s `SYS_FSYNC`, which reaches
`Partition::write_blocks` seconds after the wedge and is panicked at 500M spins:

```
[39.941 cpu4] PANIC: DEADLOCK at src/block.rs:182:20: 500M spins, ticket=2676 now=2675
    <kernel::block::Partition>::write_blocks
    <toyos_fat32::fs::Fat32<..>>::alloc_cluster
    kernel::object::ops::fsync
    kernel::arch::syscall::gate::syscall_entry
```

The panic lands in a syscall, so `percpu::in_syscall` makes it recoverable:
`try_recover_from_panic` ends that thread and returns to the scheduler, where
`deadline::wedge_if_staged` folds the CPU into the wedge. `apic::halt_all_cpus`
is therefore never reached, and neither is `deadline::stand_down` — which is why
the boot deadline and not the panic path's own bound ends the machine. That
composition is correct; what is not is that a boot staged to measure a *device*
silently loses its log writer, and the only account of it is a `PANIC` record in
a ring tail nobody was draining.

Two things are true and neither is decided here:

- The detector firing is right in general — a lock held for ever by a CPU that
  will not run again is a deadlock, and the detector cannot know the wedge is
  deliberate.
- A wedge arm that means to hold a driver's lock for the rest of the boot is
  telling the detector something it has no way to hear.

## Where it bites

Any actuator that stops a CPU inside a driver — today the `usb-wedge-*` arms,
which are QEMU registrations and reach no flashed image. It does not change
their verdicts, but it adds a panic, a dead `logd`, and a page of dropped
records to every one of them.

## Exit condition

Either a wedge declares the locks it is about to hold so the detector can leave
them alone, or the detector's refusal names the staged wedge and halts the
spinner instead of panicking it. A run of any `usb-wedge-*` arm then carries no
`DEADLOCK` record.
