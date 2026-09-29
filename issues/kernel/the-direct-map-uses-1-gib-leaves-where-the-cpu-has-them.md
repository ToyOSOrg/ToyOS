---
status: open
kind: track
opened: 2026-09-29
---

# The direct map uses 1 GiB leaves where the CPU has them

The kernel's direct map has no 1 GiB leaf: `paging::init` maps it 2 MiB at a
time (`kernel/src/arch/x86_64/paging.rs:953-956`). On a CPU that enumerates
`pdpe1gb`, CPUID.80000001H:EDX bit 26, a GiB the direct map covers whole and
that holds one memory type takes one 1 GiB leaf; the GiB that `direct_map_end`
cuts (it rounds only to 2 MiB, `toyos-bootmap/src/x86_64.rs:22`), any other
GiB, and every GiB on a CPU without it keep 2 MiB leaves. Which leaf a GiB
takes is a pure function that `paging::init` calls, living beside
`direct_map_end` in `toyos-bootmap/src/x86_64.rs` and host-tested there. The
walkers that write, `guard_4k` (`paging.rs:749`) and `ensure_table` (`:820`)
under `map_2m`, split a 1 GiB leaf into 2 MiB leaves of its type before they
descend; unsplit, `guard_kernel_page` writes a page table into the GiB's first
page. The walkers that cannot split, `policy_at` (`:718`, `&self`) and the
lock-free `debug_page_walk` (`:1098`, called by the `#PF` handler,
`idt/exceptions.rs:206,332`), read a PS=1 PDPT entry as a 1 GiB leaf and stop
there. The TCG model gains `+pdpe1gb`, which TCG implements
(`target/i386/cpu.c:945` at QEMU v11.1.1), so the PR gate maps 1 GiB leaves.

**Exit**: the decision's host test gives a GiB holding one UC range 2 MiB
leaves, the GiB a non-GiB-aligned `end` cuts 2 MiB leaves, a uniform GiB one
leaf, and every GiB 2 MiB leaves without `pdpe1gb`; under TCG with `+pdpe1gb`,
a `boot-actuators` arm reads `direct_map_policy` of a page in an unsplit 1 GiB
leaf and gets its type, finds `debug_page_walk` of that page reporting the
1 GiB leaf without descending, then guards a 4 KiB page and maps an MMIO range
inside a 1 GiB leaf, and finds the GiB's other pages mapped with their type;
each proving machine's boot reports its leaf counts; the T14's 64 KiB pipe
figure. **Mutation**, each red: a 1 GiB leaf for every GiB; a 1 GiB leaf for
the last GiB when `end` is not GiB-aligned; `guard_4k` without the split,
reading the PS=1 PDPT entry as a PD; `policy_at` without the PDPT PS check;
`debug_page_walk` without it. **Oracle**: SDM Vol. 3A §14.11.9; Linux's
`split_mem_range` (`arch/x86/mm/init.c`), which gives 1 GiB pages only to a
range's GiB-aligned interior.
