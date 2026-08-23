---
status: open
kind: defect
opened: 2026-08-24
---

# A transient block refusal during FAT cluster allocation durably splits the two FAT copies

`writeback_durability` red once on a hosted 4-core CI shard (PR #261's run), the
fatgen103 checker reporting three things at once about the `/log` volume:

```
FAT 1 differs from FAT 0 at entry 22: 0x00000000 against 0x0FFFFFFF
1 cluster(s) from 22 are marked allocated and no directory entry reaches them
FSInfo: FSI_Free_Count is 68481 and the FAT has 68480 free clusters
```

All three are one event. `toyos-fat32`'s `fat::alloc_cluster` claims a free
cluster with `set_fat_entry(cluster, END_OF_CHAIN)?` and only *then* decrements
`fsinfo.free_count`, advances `next_free`, and returns the cluster to
`append_cluster` (which links it, and whose caller writes the directory entry
later in `flush_meta`). `set_fat_entry` writes each live FAT in a loop —
`for fat in self.geom.fat_mirrors() { ... self.dev.write_at(offset, ...)? }`
(`toyos-fat32/src/fat.rs:30`) — two separate device writes with a `?` between
them. If the FAT-0 write reaches the device and the FAT-1 write is refused, that
`?` returns before the three `fsinfo` updates run (`fat.rs:165`). On the device:
FAT 0 has the cluster allocated, FAT 1 does not (mirror split); FSInfo's free
count was never decremented (stale by one); the directory entry was never
written (the cluster is leaked). Every line the checker printed.

The refusal is `BlockError::BudgetExpired` — the block layer's 2 s `OPERATION`
budget (`kernel/src/block.rs:77`), which `FatDevice::write_at` maps through
(`kernel/src/fat32_adapter.rs:293`). It is not stageable from the host
(`kernel/src/actuator.rs`, the `nvme-spent-budget` note: QEMU answers in
microseconds, so a real 2 s stall needs a real host descheduling the vCPU thread
past 2 s while a `/log` FAT write waits on the XHCI ticket lock or a USB
transfer). That is the starved hosted shard, and why it is intermittent and did
not reproduce on the dev host.

## The two composing defects, and why neither is #261

Both are in code PR #261 does not touch (it changes only `kernel/src/writeback.rs`
and `kernel/src/vfs.rs`):

1. **`set_fat_entry` is not atomic across mirrors** (`toyos-fat32/src/fat.rs`). A
   mid-loop refusal leaves the copies durably out of step, which the spec forbids
   (BPB_ExtFlags mirroring: every copy carries every update) and which is worse
   than the leaked cluster `append_cluster` documents as acceptable-and-fsck-
   recoverable: a leak wastes space; a split mirror reads differently the moment
   anything consults FAT 1. `every_fat_copy_stays_in_step` covers only the
   success path — the host `BlockAccess` cannot inject a mid-loop write failure,
   so the partial-failure path is untested. (Related but distinct:
   `issues/filesystem/what-fsck-msdos-does-not-check.md` is about the *checker's*
   blindness to a stale mirror; this is the durability of *producing* one.)

2. **The write-back drain does not retry a `BudgetExpired` flush**
   (`kernel/src/writeback.rs`). `BudgetExpired` means "not durable *yet*"
   (`block.rs`), and the retry doctrine lives in `object/ops.rs`'s `fsync` loop
   above every lock. But a file closed *without* fsync is flushed by the drain,
   and on a failed `flush_file` the drain logs and `mark_dirty_meta` re-sets the
   dirty flag — then `finish_writeback` pops the entry and tears the file down
   anyway (`file_cache::finish_writeback`). The entry is never re-enqueued, so
   the "iod/fsync will try again" promise in `Vfs::flush_file`'s comment is
   unkept for the drain path: a transient budget expiry on a close-without-fsync
   file is permanent rather than retried on a fresh budget. This behaviour is
   byte-identical between #257 and #261 (#261's `drain_all` refactor keeps the
   per-entry teardown, and its new `drain_held` runs the same `drain_one`).

## A/B measurement (dev host, 2026-08-24)

Same host, same load both arms: 12 rounds x 3 concurrent guests (staggered),
`yes`-oversubscription of 24, load average ~60. The 1 "otherfail" per arm is a
harness-side concurrent-`cargo` build race (`cannot parse -ltoyos_libc`), before
any guest boots — not a guest verdict.

| arm | guest runs | FAT-mirror failures |
|---|---|---|
| main (#257, 7ab9367b) | 35 | 0 |
| #261 kernel (`vfs.rs`+`writeback.rs`) | 35 | 0 |

Neither reproduced the corruption, as expected: the 2 s budget cannot be reached
by host-side staging on a healthy 14-core box. The rates are identical, matching
the code analysis — #261 exposed nothing #257 did not already carry, and the
mechanism predates both (it is in the FAT driver).

## A fix needs a tighter control than the checker

No actuator today fails the *second* FAT-copy write of a cluster allocation while
leaving the first durable (`usb-transport-break` fails only the first WRITE(10)
of the boot). A fix — mirror-atomicity/repair in `set_fat_entry`, and/or a
retry/re-enqueue of a `BudgetExpired` drain flush — should land with such a
negative control and the fatgen103 oracle (`toyos-fat32-check`, already what
caught this) as the independent judge.
