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
since its own last such flush. The initiator flushes its own entries by page
for up to 64 pages, and a change no other CPU runs sends no IPI. AArch64
broadcasts its invalidations in hardware and needs none of this. Every other
shootdown, a kernel-half change and a PCID reclaim among them, still reaches
every CPU, and a reached CPU still answers with a flush of every user
translation, not of the range.

What holds it: `kernel/loom/tests/tlb_tags.rs` checks the pairing a CPU
entering a space and a change reading who runs it rest on, and reds with the
pairing's fence removed; `tlb_shootdown_waits` shows on the test kernel that
a change waits for a CPU running a thread of the space and for none when no
other CPU runs one, each half red under its mutation.

What is owed, now that a thread can be pinned
(`toyos_abi::syscall::thread_set_affinity`): on a machine with PCID,
a sibling that touches a page, blocks off its pinned CPU while a thread of
another space runs there, and faults touching the page again after the
initiator unmapped it meanwhile. The TCG model has no PCID
(`build/arch.rs`), so only metal can show a switched-out CPU's entries.

In the 2026-10-28 scoreboard (evidence branch `scoreboard-2026-10-28`) the
T14 lost `mem.munmap_part.4k` at 1.22x Linux's (1,558 against 1,280 ns) and
`mem.munmap_touched_remote.4k` at 1.32x (3,028 against 2,290 ns), each
resolved; the AMD won both.

**Exit**: that case, on each proving machine with PCID, red with the flush on
entry removed; and the T14's munmap and `vm_protect` figures against Linux.
