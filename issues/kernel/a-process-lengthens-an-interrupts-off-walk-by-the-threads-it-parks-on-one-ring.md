---
status: assigned
kind: defect
opened: 2026-09-30
---

# A process lengthens an interrupts-off walk by the threads it parks on one ring

Held by the small-kernel track's stage 6 step 2
(`issues/kernel/the-kernel-is-small-interrupts-post-and-threads-wait.md`),
whose instrument is the only thing that can read it.

Every poll ring's own watch sits behind an `IrqLock`
(`kernel/src/inbox/mod.rs`), because a device handler's post
reaches it through the polls it fires. So any process, not only a device's
holder, decides how long a CPU runs with interrupts masked:

- **N threads parked in `submit` on one ring**
  are N registrations on its watch. Every completion into that ring posts the
  watch in place, which notifies all N under the list lock
  (`toyos-sched/src/watch.rs`), each a word exchange and, for a parked
  thread, a mailbox push and perhaps an IPI (`toyos-sched/src/park.rs`).
  Each woken thread's unregister is a `position` and a
  `remove` over the N, and a registration
  that finds the list full copies it, all with interrupts masked.
- **A claim's holder polling its claim from R rings, P polls each** (up to
  `MAX_PENDING_WATCHES`, 1024) makes its device's
  handler fire R × P entries under the claim's list lock, each posting its
  ring's watch, whose own N threads it notifies. Entries a post in place
  fired stay in the list until
  registrations sweep them four at a time.

Nothing caps N: a thread costs its process a 128 KiB kernel stack
(`kernel/src/process.rs`) and no count. The first walk starts in
`SYS_INBOX_SUBMIT`, and a syscall runs with interrupts masked from entry to
exit (`issues/kernel/syscall-preemption-is-incidental.md`), with #634 and
without it: #634 did not mask it, and reverting #634 does not shorten it.
The second runs in the device's handler since #634.

How long the second is has not been read. The first has, at one size: the
three T14 boots of #649 at `8b73eba69` (comment 5959415453, readbacks
`649-r5/1-head`, `649-r5/2-report-halved`, whose kernel prints half of every
span, and `649-r5/3-idle-halt-counted`) ran it with N = 256
(`test_rs_ring_park_herd`) and read it from the herd's own report, which
spans the runner's spawn of the herd as well. The longest `irqs_off_ns` on
any CPU there is 1032292 (`windowscase/kernel.log:428`), 2 × 519358 (`:426`)
and 1175943 (`:430`). The third is cpu7's, the CPU that spawned the herd
(`issues/kernel/the-cpu-that-spawns-a-toybox-applet-reads-1-4-ms-of-interrupts-and-preemption-off-on-the-t14.md`),
and the other seven read 257646 to 364953 in that boot.

The three at `0aa8d4c88` (comment 5960575031, readbacks `649-r6/1-head`,
`649-r6/2-report-halved` and `649-r6/3-idle-halt-counted`, the same two
mutated kernels) read the same report of the same load at 1980239
(`windowscase/kernel.log:430`), 2 × 5075290 (`:422`) and 4725822 (`:416`):

- 1980239 is cpu7's, which spawned the herd (`:395`). The other seven read
  969922 to 1044783.
- 2 × 5075290 is one of eight: every CPU reads 2 × 5014552 to 2 × 5075290
  (`:416` to `:430`), beside `tlb: shootdowns=282 wait=131633us max=10038us`
  (`:431`). It is the first reading of
  `issues/kernel/some-t14-boots-carry-an-interrupts-off-window-of-9-ms-or-more.md`.
- 4725822 is cpu0's. The other seven read 544283 to 1018355.

So one size reads 1.03 to 10.15 ms over six boots, and which boot carried
one of the machine's own events decides a single reading. None of the six
took an interrupt a handler #634 changed serves
(`userdev=0 sound=0 dmafault=0 hda=0` on every CPU).

**Exit**: the interrupts-off window step 2's instrument reads on the T14
under N threads parked in `submit` on one ring, a sibling thread completing
into it, does not grow with N: read at two sizes on one kernel, each size the
median of at least five boots, each boot from the load's own report, with
the tail reported beside it. `ring_park_herd`'s sibling waits on the kernel's
roster, which is itself a masked walk over every thread of the machine (3,619
to 4,111 `SYS_SYSINFO` calls a run in #649's fourteen T14 boots, and 768 to
847 ms of syscall wall in a run of 0.9 s), so that reading has to tell the
two walks apart.
