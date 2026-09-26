---
status: open
kind: defect
opened: 2026-09-26
---

# blockd would serve the partition the machine is running from

blockd (`userland/blockd/src/main.rs`) opens any partition of its disk whose
table entry checks out, to whoever holds its port. The kernel's own claims
refuse the running ROOT slot (`rootfs::hold_source`), because a writer there
can change the image under the kernel that booted from it; blockd has no such
refusal, and nothing tells it which of its partitions the machine booted.

It cannot bite today: blockd drives a controller the kernel does not, and the
boot medium is always the kernel's (the stick, or the first NVMe controller).
It bites the day blockd drives the disk the loader read ROOT from — the
small-kernel track's step 9 (`issues/kernel/the-kernel-is-small-interrupts-post-and-threads-wait.md`),
or an installed machine booting off NVMe once the kernel stops driving it.

**Exit condition.** blockd learns the running slot's unique GUID from the
loader's handoff, through whatever starts it, and refuses a session on it by
name; a guest test boots with the running ROOT on blockd's disk and sees the
open refused while the idle slot opens.
