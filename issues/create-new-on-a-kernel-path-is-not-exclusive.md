---
status: open
kind: defect
opened: 2026-09-27
---

# `create_new` on a kernel path is not exclusive

std's `OpenOptions::create_new(true)` and C's `open` with
`O_CREAT|O_EXCL` promise an open that fails when the file is there
(`AlreadyExists`, `EEXIST`). Both reach `toyos_fs_open`'s kernel arm
(`sdk/toyos/src/files/owner.rs`), which turns `create_new` into the kernel's
plain `OpenFlags::CREATE`, and the kernel has no exclusive flag to turn it
into. So on `/tmp` a second exclusive create of one path opens the file the
first made and says nothing, and a C lock file made that way locks nothing. A
served path does not share it: the file protocol carries `O_CREATE_NEW` and
the file server refuses. Found when the file server's test actuator took two
`create_new`s of one `/tmp` mark from two processes as two first times.

**Exit**: an exclusive create reaches the kernel's `/tmp` and is refused
`AlreadyExists` there, with a guest test that makes one path twice with
`create_new` and with `O_CREAT|O_EXCL` and has each second one refused.
