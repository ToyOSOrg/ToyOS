---
status: open
kind: track
opened: 2026-09-29
---

# The direct map uses 1 GiB leaves where the CPU has them

The kernel's direct map has no 1 GiB leaf: `paging::init` maps it 2 MiB at a
time (`kernel/src/arch/x86_64/paging.rs:953-956`). On a CPU that enumerates
`pdpe1gb`, CPUID.80000001H:EDX bit 26, a GiB that holds one memory type takes
one 1 GiB leaf; any other GiB, and every GiB on a CPU without it, keeps 2 MiB
leaves. Which leaf a GiB takes is a pure function that `paging::init` calls,
living beside `direct_map_end` in `toyos-bootmap/src/x86_64.rs` and host-tested
there. Every walker of the direct map reads a PDPT entry as a table pointer —
`policy_at` (`paging.rs:718`), `guard_4k` (`:749`), `ensure_table` (`:820`)
under `map_2m` — so each splits a 1 GiB leaf into 2 MiB leaves of its type
before it descends; unsplit, `guard_kernel_page` writes a page table into the
GiB's first page. The TCG model gains `+pdpe1gb`, which TCG implements
(`target/i386/cpu.c:945` at QEMU v11.1.1), so the PR gate maps 1 GiB leaves.

**Exit**: the decision's host test gives a GiB holding one UC range 2 MiB
leaves, a uniform GiB one leaf, and every GiB 2 MiB leaves without `pdpe1gb`;
under TCG with `+pdpe1gb`, a `boot-actuators` arm guards a 4 KiB page and maps
an MMIO range inside a 1 GiB leaf, and finds the GiB's other pages mapped with
their type; each proving machine's boot reports its leaf counts; the T14's 64
KiB pipe figure. **Mutation**, each red: a 1 GiB leaf for every GiB; `guard_4k`
without the split, reading the PS=1 PDPT entry as a PD. **Oracle**: SDM Vol. 3A
§14.11.9.
