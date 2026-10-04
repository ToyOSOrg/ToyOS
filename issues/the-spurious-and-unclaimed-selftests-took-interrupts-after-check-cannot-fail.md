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

Owner: the LAPIC selftests, `kernel/src/arch/x86_64/idt/`.

**Exit:** a mutation that leaves the CPU taking no interrupt once the probe's
handler returns makes each selftest print `FAILED`, and the `selftests` row in
`tests/toyos.rs` that boots both actuators reds on it.
