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

- **Owed on metal:** the idle holder (#1251) on the T14: `tlb_shootdown_waits`
  in the `shared-debug` boot, green, its third part counting zero CPUs
  reached by a change that frees no table. On the T14 and the AMD bench, no
  `testcases`, `shared-debug` or `testcases-watchdog` boot of main ends on
  `tlb: a CPU still holds the root` or `paging: a space's tables are freed
  while a CPU still holds its root`.

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

- **Owed on metal** for the asks above (a reached CPU flushing only the
  change's range, the initiator owing itself nothing):
  - The T14's `shared-debug` boot (the actuator kernel), where main must pass
    `test_rs_tlb_shootdown_waits`: a change still waits for every CPU running
    the space.
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
