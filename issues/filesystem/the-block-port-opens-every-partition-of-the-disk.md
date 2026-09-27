---
status: open
kind: defect
opened: 2026-09-26
---

# The block port opens every partition of the disk

A holder of blockd's `block` connector (`toyos_blockring::PORT`) may open a
session on any partition blockd serves, named by nothing but its unique GUID
(`userland/blockd/src/main.rs`, `Service::open`). The kernel's partition claims
are minted one partition at a time from a manifest row, so a program the
manifest gives one partition reaches that one; a program given blockd's port
reaches the whole disk.

Not fixed with blockd's first landing because nothing yet hands the port to a
program: no manifest row starts blockd, and its only client is the test that
supervises it. Scoping it means the authority to open a partition has to be
something init mints per row — a connector per partition, or a session
capability blockd issues and init hands on — which is the manifest wiring the
small-kernel track's steps 6 and 7 build
(`issues/kernel/the-kernel-is-small-interrupts-post-and-threads-wait.md`).

**Exit condition.** What a program holds names the partitions its manifest row
gives it and no others, and a guest test holding one partition's authority is
refused another partition of the same disk by name.
