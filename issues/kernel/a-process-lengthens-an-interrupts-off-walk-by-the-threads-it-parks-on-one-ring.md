---
status: assigned
kind: defect
opened: 2026-09-30
---

# A process lengthens an interrupts-off walk by the threads it parks on one ring

Held by the small-kernel track's stage 6 step 2
(`issues/kernel/the-kernel-is-small-interrupts-post-and-threads-wait.md`),
whose instrument is the only thing that can read it.

Every poll ring's own watch and its completions sit behind an `IrqLock`
(`kernel/src/inbox/mod.rs:206`, `:208`), because a device handler's post
reaches them through the polls it fires. So any process, not only a device's
holder, decides how long a CPU runs with interrupts masked:

- **N threads parked in `submit` on one ring** (`kernel/src/inbox/mod.rs:498`)
  are N registrations on its watch. Every completion into that ring posts the
  watch in place (`:326`), which notifies all N under the list lock
  (`toyos-sched/src/watch.rs:189`), each a word exchange and, for a parked
  thread, a mailbox push and perhaps an IPI (`toyos-sched/src/park.rs:117-126`).
  Each woken thread's unregister is a `position` and a
  `remove` over the N (`toyos-sched/src/watch.rs:140-144`), and a registration
  that finds the list full copies it (`:213`), all with interrupts masked.
- **A claim's holder polling its claim from R rings, P polls each** (up to
  `MAX_PENDING_WATCHES`, 1024, `kernel/src/inbox/mod.rs:199`) makes its device's
  handler fire R × P entries under the claim's list lock, each taking that
  ring's completions lock and posting that ring's watch, whose own N threads
  it notifies. Entries a post in place fired stay in the list until
  registrations sweep them four at a time.

Nothing caps N: a thread costs its process a 128 KiB kernel stack
(`kernel/src/process.rs:236`) and no count. Before #634 every one of these
walks ran with interrupts open, under preemption off. By reading, not
measured: no instrument in the tree reads an interrupts-off window.

**Exit**: step 2's interrupts-off window, read on the T14 while one process
parks 256 threads in `submit` on one ring and a sibling thread completes into
it, is no longer at step 1's head than at stage 6's first commit under the
same load.
