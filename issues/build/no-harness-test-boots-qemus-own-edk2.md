---
status: open
kind: tooling
opened: 2026-09-28
---

# No harness test boots QEMU's own edk2

Every harness boot uses the repository's `ovmf/`. QEMU's own
`edk2-x86_64-code.fd` hands an AMD vCPU a reserved range at
`0xfd00000000..0x10000000000`, which `ovmf/` never names, and a kernel whose
direct map reached every range in the map died there after `pmm:`, and a
survey that offered a 64-bit BAR only the space above that range handed the
NIC over nowhere. The host tests `toyos-bootmap/tests/direct_map.rs` and
`toyos-pci`'s `placement` hold the two rules; nothing boots the
firmware that broke them, so a regression outside those rules is seen by the
first person who boots QEMU's firmware and nobody else.

Owner: orchestrator. Exit condition: a harness test boots QEMU's own edk2 on
the command line the release probe uses and reaches `compositor: ready` and `netd: DHCP: lease`.
