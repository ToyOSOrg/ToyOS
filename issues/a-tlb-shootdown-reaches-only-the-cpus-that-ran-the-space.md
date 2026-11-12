---
status: open
kind: track
opened: 2026-09-29
---

# A TLB shootdown reaches only the CPUs that ran the space

On x86 a change to one user space's leaves now reaches only the CPUs running
that space (`kernel/src/arch/x86_64/tlb.rs`, `user_change`, over
`kernel/src/tlb_tags.rs`). A CPU that switched away keeps the space's entries
under PCID, since every CR3 load sets NOFLUSH, so it is not reached; it
flushes every user translation before it next enters a space that changed
since its own last such flush. A CPU whose idle context kept the space's root
still runs it, but a change that frees no table passes it by and it flushes
when a thread of the space resumes there; one that frees a table, or the
space's end, reaches it, and its answer loads the kernel's root. A change no
other CPU runs sends no IPI. Each CPU reached is asked the change's range,
from the initiator's own mailbox, and answers that initiator alone: it
flushes the range by page, up to 64 pages, or every user translation when
the range is longer or it was asked by more than one initiator at once. So a
CPU no change asks owes no flush, and the initiator owes itself none: it
sends its IPIs first and then flushes its own entries by page while they fly.
Only a flush of every user translation counts as the CPU's last such flush.
AArch64 broadcasts its invalidations in hardware and needs none of this.
Every other shootdown, a kernel-half change and a PCID reclaim among them,
still reaches every CPU, which answers with a flush of every user
translation.

What holds it: `kernel/loom/tests/tlb_tags.rs` checks the pairing a CPU
entering a space, or resuming one its idle context kept, and a change reading
who runs it rest on, and reds with either pairing's fence removed;
`kernel/loom/tests/tlb_shootdown.rs` checks that an answered ask flushed its
range after the write, alone or with a second initiator's, and that a mailbox
reused waits for its new answer, each red under its mutation;
`tlb_shootdown_waits` shows on the test kernel that a change waits for a CPU
running a thread of the space, and that one freeing no table waits for none
when no other CPU runs one, each half red under its mutation.

What is owed, now that a thread can be pinned
(`toyos_abi::syscall::thread_set_affinity`): on a machine with PCID,
a sibling that touches a page, blocks off its pinned CPU while a thread of
another space runs there, and faults touching the page again after the
initiator unmapped it meanwhile. The TCG model has no PCID
(`build/arch.rs`), so only metal can show a switched-out CPU's entries.

In the 2026-10-28 scoreboard (evidence branch `scoreboard-2026-10-28`) the
T14 lost `mem.munmap_part.4k` at 1.22x Linux's (1,558 against 1,280 ns) and
`mem.munmap_touched_remote.4k` at 1.32x (3,028 against 2,290 ns), each
resolved; the AMD won both. On 2026-11-11 at ad5db5a65 ([evidence](https://github.com/ToyOSOrg/evidence/tree/d44ebdae1f9f584d7b826e1498dbfd791a8d2339),
four boots against five Linux runs) the T14 read `mem.munmap_part.4k` at
1.12x (1,406 against 1,254 ns) and `mem.munmap_touched_remote.4k` at 1.47x
(3,341 against 2,279 ns), each resolved, and `mem.mprotect.4k` ahead (816
against 1,019 ns); the AMD bench is ahead on all three.

Stage probes at 31eb5416b, before the asks above, one boot a bench ([evidence](https://github.com/ToyOSOrg/evidence/tree/cf6a882c04a646a8998e3bf0ad66e9f699b8e8aa/memperf/diagnosis)),
put the shootdown at about 900 of the T14's 3,341 ns for
`mem.munmap_touched_remote.4k` and 750 on the AMD bench. Most of the rest
was the inventory's removal (1,004 ns) and a walk of the futex buckets
(1,159 ns), which an unmap no longer makes while no thread of its space
waits. Of the shootdown, on
the T14:

- 542 ns from the IPI's send to the target's answer. Inside it the
  initiator flushed every user translation of its own, 200 ns, in every one
  of 2,050 unmaps: its wait served the generation it had just issued, though
  it had flushed its pages one by one already (92 ns).
- 63 ns to send the IPI, 64 to issue the generation, 52 to read who runs
  the space.
- The reached CPU flushed every user translation, 254 ns, where the change
  was one page.

The generation was machine-wide, so a CPU the change did not target owed a
flush of every user translation at its next lock spin or its own next
shootdown's wait. No such flush was seen in these rows, whose other CPUs sat
idle. No timed unmap issued the second shootdown a clear makes when tables
go: best fit placed each page in a span that kept its tables.
`mem.munmap_part.4k` flushes nothing; its time is the inventory's
(`issues/user-memory-hot-paths-are-slower-than-2-mib-pages.md`).

- **Owed on metal** for the asks above (a reached CPU flushing only the
  change's range, the initiator owing itself nothing) and the idle holder
  (#1251):
  - The T14's `shared-debug` boot (the actuator kernel), where main must pass
    `test_rs_tlb_shootdown_waits`: a change still waits for every CPU running
    the space, and its third part counts zero CPUs reached by a change that
    frees no table.
  - On the T14 and the AMD bench, no `testcases`, `shared-debug` or
    `testcases-watchdog` boot of main ending on `tlb: a CPU still holds the
    root` or `paging: a space's tables are freed while a CPU still holds its
    root`.
  - An A/B on both benches of `osbench_system` and `osbench_mem`, main from
    before and after. Main after must read `mem.munmap_touched_remote.4k`
    faster, and `mem.munmap_part.4k`, `mem.mprotect.4k` and the multi-thread
    `mem.malloc_churn.*` rows no slower. Until then the gain is unmeasured.
  - With it, each bench's cycles per flush, one page by `INVPCID` (the T14)
    or `INVLPG` (the AMD bench, which has no PCID) against one flush of every
    user translation. From those, the number of pages at which flushing one
    by one costs as much as a full flush; the reached CPU's limit, 64, is
    lowered if that number is well below it, or the reason not to is
    recorded. Branches `memperf-sd-ab-base` and `memperf-sd-ab-head` carry a
    measurement-only probe that logs both; they were staged and never booted.

**Exit**: that case, on each proving machine with PCID, red with the flush on
entry removed; and the T14's munmap and `vm_protect` figures against Linux.
