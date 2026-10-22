---
status: open
kind: track
opened: 2026-10-16
---

# ToyOS hosts hardware-accelerated virtual machines

Owner decision, 2026-10-16: hosting other operating systems, such as Linux,
in hardware-accelerated virtual machines is a later product goal. Native
ToyOS applications remain the priority. This does not schedule implementation
now or justify speculative virtualization hooks in current work.

The kernel/userland boundary, supported hardware and guests, VM interfaces,
device models and resource isolation need a concrete design before
implementation. Apply the existing userland-first and capability principles;
no particular hypervisor implementation or API is selected.

ToyOS running as a guest is owned by
`issues/toyos-runs-in-any-open-source-hypervisor.md`. Native Linux binary
compatibility remains a separate deferred target under
`issues/a-package-is-a-directory-under-apps-and-the-installer-is-a-program.md`.
