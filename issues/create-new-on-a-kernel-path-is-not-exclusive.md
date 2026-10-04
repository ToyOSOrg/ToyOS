---
status: open
kind: defect
opened: 2026-09-27
---

# `create_new` on a kernel path is not exclusive

std's `OpenOptions::create_new(true)` promises an open that fails with
`AlreadyExists` when the file is there. The std fork's `to_flags`
(`rust/library/std/src/sys/fs/toyos.rs`) turns `create_new` into the kernel's
plain `OpenFlags::CREATE`, and the kernel has no exclusive flag to turn it
into, so on `/tmp` a second `create_new` of one path opens the file the first
made and says nothing. A served path does not share it: the file protocol
carries `O_CREATE_NEW` and fsd refuses. Found when fsd's test actuator took two
`create_new`s of one `/tmp` mark from two processes as two first times.

**Exit**: an exclusive create reaches the kernel's `/tmp` and is refused
`AlreadyExists` there, with a guest test that makes one path twice with
`create_new` and has the second refused.
