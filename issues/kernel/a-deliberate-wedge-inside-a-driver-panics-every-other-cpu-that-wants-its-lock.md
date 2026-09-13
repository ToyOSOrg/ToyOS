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

On the T14, run 39, that cost a thread. `logd`'s `SYS_FSYNC` reached
`Partition::write_blocks` 3.7 s after the wedge, spun on the partition lock, and
`sync::Lock`'s own deadlock detector panicked it at 500M spins:

```
[ 6.216 cpu4] LOCK CONTENTION: 50M spins at src/block.rs:182:20, ticket=2652 now=2651
[39.734 cpu4] LOCK CONTENTION: 500M spins at src/block.rs:182:20, ticket=2652 now=2651
[39.734 cpu4] PANIC: DEADLOCK at src/block.rs:182:20: 500M spins, ticket=2652 now=2651
    <kernel::block::Partition>::write_blocks
    <toyos_fat32::fs::Fat32<..>>::alloc_cluster
    kernel::object::ops::fsync
    kernel::arch::syscall::gate::syscall_entry
```

The panic landed in a syscall, so `percpu::in_syscall` made it recoverable:
`try_recover_from_panic` ended that thread and returned to the scheduler, where
`deadline::wedge_if_staged` caught the CPU and folded it into the wedge.
`apic::halt_all_cpus` was therefore never reached — and with it neither
`deadline::stand_down` nor `panic_reboot::arm`, which is why the machine was
ended at 120062 ms by the boot deadline rather than at ~99.7 s by the panic
path's own minute. That composition is correct; what is not is that a boot
staged to measure a *device* silently lost its log writer 37 s in, and that the
only account of it is a `PANIC` record in a ring tail nobody was draining.

Two things are true and neither is decided here:

- The detector firing is right in general — a lock held for ever by a CPU that
  will not run again is a deadlock, and the detector cannot know the wedge is
  deliberate.
- A wedge arm that means to hold a driver's lock for the rest of the boot is
  telling the detector something it has no way to hear.

## Where it bites

Any actuator that stops a CPU inside a driver, which is now three of the five in
`toyos_build::metal::WEDGE_ARMS`. It does not change those arms' verdicts —
`stick_secs` is measured by the next host, not by the wedged kernel — but it
adds a panic, a dead `logd`, and 138 dropped records to every one of them.

## Exit condition

Either a wedge declares the locks it is about to hold so the detector can leave
them alone, or the detector's refusal names the staged wedge and halts the
spinner instead of panicking it. A run of any `usb-wedge-*` arm then carries no
`DEADLOCK` record.
