---
status: open
kind: defect
opened: 2026-10-28
---

# The scratch disk `--adopt-scratch` makes is not served

`toyos-metal --adopt-scratch` (`tests/metal/src/lib.rs`, `Job::Scratch`)
writes a table whose one partition runs from LBA 2048 to the last usable
sector (`sgdisk --new=1:0:0`), so on a stick whose sector count is not whole
4 KiB blocks the partition is not whole blocks either, and usbd offers it
and refuses every session on it by name: a bench boot's log read
`partition … of type 51CC09F9-7442-435B-BC03-0FD8718B0FB5 is not served: LBA
2048+60060419 is not whole 4 KiB blocks of 512`. A destructive test given
that stick has nothing to write.

`--adopt-scratch` now lays the partition on whole mebibytes, so a stick
adopted again is expected to be served. No stick adopted that way has been
served yet, and the T14's, adopted before, is not served until it is adopted
again.

**Exit condition:** a scratch disk is initialized from inside ToyOS with one
partition on whole 4 KiB blocks (`issues/toyos-manages-its-own-partitions.md`,
stage 3), a bench boot serves a session on it, and `--adopt-scratch` is
deleted.
