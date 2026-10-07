---
status: open
kind: defect
opened: 2026-10-04
---

# The spurious and unclaimed selftests' "took interrupts after it" check cannot fail

`spurious::selftest` (`kernel/src/arch/x86_64/idt/spurious.rs`) and
`unclaimed::selftest` (`kernel/src/arch/x86_64/idt/unclaimed.rs`) each claim to
confirm the CPU still takes interrupts after the probe vector. They read
`taken_before = deliveries_total(cpu)` before `apic::send_self`. The probe's
own delivery is counted in that total. Once `delivered` has held, the probe's
source count is past `before`, so `deliveries_total(cpu) > taken_before` is
already true at the first poll. A CPU left deaf by the probe's handler passes
the check, and the `3/3` line says "the CPU took interrupts after it" either
way.

The code fix is in `main`: each selftest arms the BSP's one-shot with
`arm_within` once the probe's handler has returned, and waits for the timer's
own count (`Source::Timer`) to move. Never a total: every metal image carries
a boot deadline, so the hard-lockup sampler's performance-counter NMI is armed
on the BSP before the selftests run, and an NMI is counted whatever `IF`
holds. The exit is unread on hardware: no QEMU guest test arms either
selftest, and the T14 row `lapic_spurious_vector` has not booted the fix. The
mutation is `selftests-deaf-after-probe-on-head.patch` (both gates `iretq`
with IF cleared), and its negative control
`selftests-deaf-after-probe-on-base.patch`, both posted on the pull request
that landed the fix (#741). Until the row is read, that the check can fail on
the T14 is still a reading of the code.

Owner: the LAPIC selftests, `kernel/src/arch/x86_64/idt/`.

**Exit:** three T14 runs of `lapic_spurious_vector` are read. The fixed head
is green, both selftests `3/3` with `(0 -> 1)`; the head with
`selftests-deaf-after-probe-on-head.patch` is red, on both `… did not take
the timer interrupt armed after …` lines; the head with
`selftests-deaf-after-probe-on-base.patch`, the fix reverted under the same
mutation, is green, the defect the fix removes.
