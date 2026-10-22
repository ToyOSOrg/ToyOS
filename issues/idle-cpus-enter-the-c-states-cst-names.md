---
status: open
kind: track
opened: 2026-09-29
---

# Idle CPUs enter the C-states `_CST` names

At primary revision `0e6716fc0`, idle entry is still `hlt`/`wfi`; firmware
idle-state publication and deeper entry remain to implement. `acpiserver`
already contains AML loading/evaluation, so do not treat absence of the whole
interpreter as a current prerequisite. Extend that service to publish validated
processor-state facts and check its actual coverage before dispatching.

The owner-approved contract now lives in
`issues/the-scheduler-and-the-clocks-never-talk.md`. Reconcile #870 before
implementation. Its old machine-wide realtime tolerance and blanket permission
for timers to arrive one exit-latency late are superseded.

**Exit:** supported hardware states, safe timer continuity and race-free entry
respect the actual wake-path latency requirements. Validate on the T14 and AMD
benches, recording available states and remaining hardware limitations. Host
tests establish selection policy; metal establishes entry and wake behavior.
Measure residency and package energy separately: correct core sleep does not
alone prove package power savings. Linux on the same machine is a comparison,
not a correctness oracle for every timing contract.
