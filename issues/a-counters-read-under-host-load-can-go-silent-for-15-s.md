---
status: open
kind: defect
opened: 2026-10-04
---

# A counters read under host load can go silent for 15 s on an 8-CPU `virt` guest

`test_rs_counters_read` (`tests/images/virtsmpcase.toml`) goes quiet past the
harness's 15 s `GUEST_QUIET` when the host starves the guest's vCPUs, and
`virt_smp`, `virt_el1_smp`, `virt_mask_windows` and
`virt_off_names_the_cpus_left_on` red as `STALLED`. It was sighted eleven
times from 2026-10-04 to 2026-11-02, at host loads of 30 to 85, on TCG and on
HVF.

**What the silent guest is doing.** Every vCPU is in `sync::Lock::lock` on the
reading process's space lock, from `counters::read` writing its records out
through `user_ptr::View::pieces`. The lock is not stuck. Read over QEMU's
gdbstub a few seconds apart during the silence, its `now` advances about 180
handovers a second, and `ticket - now` stays 8, with every vCPU queued. A ticket
lock hands over to one particular waiter. While the host has that waiter's vCPU
descheduled, every other vCPU waits for it. The read finishes after the harness
has stopped listening. No holder is off-CPU. An earlier reading of a 5M-spin
scratch kernel's panics said otherwise, but its context table was printed after
`now` had passed the tickets it named.

**Measured beside other guests, unclamped.** Six interleaved rounds of the
four rows ran at host load 18 to 51 on the 14-core dev host. Each read's span
was measured on the guest's clock.

| kernel | rows silent | spans of the reads that ended |
|---|---|---|
| main, nine space-lock takes a read | 8 of 24 | 0.85 to 14.54 s |
| one copy a read, two takes | 2 of 24 | 0.87 to 12.55 s |
| scratch test-and-test-and-set `sync::Lock` | 0 of 24 | 0.76 to 1.57 s |

The third arm changes only how the lock hands over, which makes the FIFO
handover the cause. One copy per read (`counters::read`) makes the queue
shorter, but the queue remains.

**What remains.** The ticket lock stays, as decided on 2026-11-04: its FIFO
fairness is what metal gets, and QEMU must never reduce what metal gets. Any
`sync::Lock` that every vCPU queues on still runs at the host's scheduling pace
when the host starves the guest. The guest is starved because the harness runs
an 8-vCPU guest beside others on a host that cannot give it eight cores.

Exit: the harness admits a guest only where the host has a CPU for each of its
vCPUs, and the four rows then pass repeatedly beside other guests under the
parallel load that silenced them.
