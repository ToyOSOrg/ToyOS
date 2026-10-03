---
status: open
kind: tooling
opened: 2026-10-03
---

# `host_scale` reads host speed off boots that stop at different markers

`budget_smp` (`tests/common/qemu.rs`) pays a ceiling out for a slower host by
`host_scale`: the run's fastest boot against `REFERENCE_BOOT_MS`, 1424 ms.
`record_boot` takes each boot's time to its own ready marker, and those differ:
`virt_early_panic`'s and `virt_early_fault`'s is `EARLY PANIC:`, which the
kernel prints when it panics early in bring-up; the other AArch64 boots' is
`control registers: SCTLR_EL1=`; an x86-64 boot's is `===READY===`, after
userland starts. The boots also run under different accelerators
(`Profile::accel`). So the scale says which boots a run held, not how fast the
host is: a run with an early-panic boot in it pays 1× on nearly any host. With
the suite's width no longer multiplying a ceiling, `host_scale` and
`oversubscription` are the only host corrections `budget_smp` makes.

## Measured

On the dev host, an arm64 Mac, one run after the other at `e5daa61ff`, load
average 1.7 to 2.8:

| command | the boot | its fastest boot | scale |
|---|---|---|---|
| `cargo test --test toyos-build -- virt_early_panic` | `Virt`, HVF, to `EARLY PANIC:` | 479 ms | 1.00× |
| `cargo test --test toyos-build -- virt_smp` | `VirtEl2`, TCG, to `SCTLR_EL1=` | 1180 ms | 1.00× |
| `cargo test --test toyos-build -- machine_shutdown` | x86-64, TCG, to `===READY===` | 3316 ms | 2.33× |

The whole suite at `e5daa61ff`, 12 wide on the same host, read 566 ms and paid
1.00×.

## Owner

`issues/build/the-tooling-is-a-review-prompt-and-three-workflows.md`, whose
harness item keeps a piece of the harness only where it sees what reading
cannot.

## What would close it

`host_scale` reads only boots of one profile that stop at one marker, or it is
deleted with `budget_smp`'s promise of it.
