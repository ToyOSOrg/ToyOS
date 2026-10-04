---
status: open
kind: tooling
opened: 2026-09-29
---

# No test can hold a thread on a named CPU

No `boot-actuators` arm and no `SYS_DEBUG` action pins a thread, so no test
can put two threads on two CPUs it knows. The pin is a `test-actuators`
action, so the shipped kernel carries none.

**Exit**: a guest test pins a thread to each CPU in turn and reads its x2APIC
ID from CPUID.0BH:EDX across 1000 yields at each, on every proving machine and
under TCG: one ID per pin, a different one per CPU. **Mutation**: a pin the
scheduler's placement ignores. **Oracle**: the CPU's own x2APIC ID.
