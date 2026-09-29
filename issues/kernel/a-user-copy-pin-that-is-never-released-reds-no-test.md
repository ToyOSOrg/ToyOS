---
status: open
kind: defect
opened: 2026-09-29
---

# A user-copy pin that is never released reds no test

`kernel/src/user_ptr.rs`'s `impl Pins for Pmm` is the one pin path no host
test reaches: the host tests of `toyos_userbound::Pinned` drive a counting
fake. With its `unpin` emptied (`fn unpin(&mut self, _run: Segment) {}`),
every frame a syscall ever copied through keeps a pin. `pmm::free_page` still
counts such a frame free, and `alloc_page` and `alloc_contiguous` skip it for
the rest of the boot, so the machine loses memory that no free count shows and
no known guest test reads.

## Exit condition

A test that goes red under that mutation: for example, a guest job that copies
into and unmaps more frames than the guest has, or a census that counts free
frames still holding a pin and a test that asserts it returns to zero once no
copy is in flight. Then this file is deleted.

## Owner

`kernel/src/user_ptr.rs`, `kernel/src/mm/pmm.rs`. Nobody holds it.
