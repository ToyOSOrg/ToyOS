---
status: open
kind: tooling
opened: 2026-09-28
---

# No harness test boots QEMU's own edk2

Every harness boot uses the repository's `ovmf/`. QEMU's own
`edk2-x86_64-code.fd` hands an AMD vCPU a reserved range at
`0xfd00000000..0x10000000000`, which `ovmf/` never names, and a kernel whose
direct map reached every range in the map died there after `pmm:`. The host
test `toyos-bootmap/tests/direct_map.rs` holds the rule; nothing boots the
firmware that broke it, so a regression outside that rule is seen by the
first person who boots QEMU's firmware and nobody else.

Owner: orchestrator. Exit condition: a harness test boots QEMU's own edk2 on
the command line the release probe uses and reaches `compositor: ready`.
