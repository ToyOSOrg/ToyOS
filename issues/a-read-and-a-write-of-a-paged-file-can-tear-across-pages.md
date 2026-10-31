---
status: open
kind: defect
opened: 2026-10-31
---

# A read and a write of a paged file can tear across pages

POSIX XSH 2.9.7 makes a `read` and a `write` of a regular file atomic with
respect to each other. A holder's `SYS_PAGED_READ` copies a pinned frame at a
time (`kernel/src/object/paged.rs`), and the file server's write lands page
by page under write pins, outside the object's lock. A read concurrent with a
write of several pages can return some pages before the write and some after
it, and a page can be torn at the bytes being copied.

This is the contract memory stage 7a states (writes are visible at once, with
no atomicity beyond the hardware's), and it is not what POSIX asks.

**Exit**: a read and a write of one file are atomic with respect to each
other, or each is atomic per call by a rule the file server and the kernel
state together, with a guest test that reds on a torn read.

**Owner**: the storage track (`issues/storage-is-layers-and-a-role-is-a-filesystem.md`).
