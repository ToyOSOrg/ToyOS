---
status: open
kind: defect
opened: 2026-10-27
---

# A refused release of a large block leaks it

std's and libc's allocators free a block of 256 KiB or more with `vm_release`
(`sdk/libc/src/mapped.rs`), and nothing can be done with a refusal. The block
stays mapped, committed and unreachable, so its frames and its promise are lost
until the process ends.

The release is refused in two ways:
- At `MAX_RANGES`.
- When the space's range charge cannot grow (`AddressSpace::room`, then
  `Charge::resize`, then `pmm::charge`). `pmm::charge` refuses exactly when
  memory is low.

Freeing a block from the middle of one inventory entry cuts it, because
adjacent large blocks and heap segments that continue each other join. `room`
then counts the two cuts. When that count crosses a `RANGE_GRAIN` above what is
already charged, the release needs a new charge.

dlmalloc keeps a segment whose release is refused and reuses it. A large block
has no such fallback.

Owner: the memory track. Exit: no refusal of a large block's release loses its
memory. One way is for a release, which leaves a space at most one range
larger, to need no new charge, because every change that adds ranges charges
for two more than it leaves. A release at `MAX_RANGES` must also give the
memory back, for example by freeing the frames (`vm_discard`) and returning
the promise.
