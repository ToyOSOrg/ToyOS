---
status: open
kind: track
opened: 2026-09-29
---

# The direct map uses 1 GiB leaves where the CPU has them

The direct map has no 1 GiB leaf: `toyos-bootmap` maps 2 MiB and 4 KiB
(`toyos-bootmap/src/x86_64.rs:33-54`). On a CPU that enumerates `pdpe1gb`,
CPUID.80000001H:EDX bit 26, a GiB that holds one memory type takes one 1 GiB
leaf; any other GiB, and every GiB on a CPU without it, keeps 2 MiB leaves.
The TCG model gains `+pdpe1gb`, which TCG implements (`target/i386/cpu.c:945`
at QEMU v11.1.1), so the PR gate maps 1 GiB leaves.

**Exit**: a `toyos-bootmap` host test gives a GiB holding one UC range 2 MiB
leaves, a uniform GiB one leaf, and every GiB 2 MiB leaves without `pdpe1gb`;
each proving machine's boot reports its leaf counts; the T14's 64 KiB pipe
figure. **Mutation**: a 1 GiB leaf for every GiB. **Oracle**: SDM Vol. 3A
§14.11.9.
