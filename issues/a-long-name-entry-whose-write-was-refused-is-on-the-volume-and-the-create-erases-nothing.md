---
status: open
kind: defect
opened: 2026-09-21
---

# A long-name entry whose write was refused is on the volume, and the create that wrote it erases nothing

Five T14 boots under `usb-transport-break` left the log partition failing
`toyos-fat32-check` the same way: `/: entry 3 is a long-name entry carrying
checksum 0xAC of a short name whose checksum is 0xB0` and `entry 3 is long-name
ordinal 2 where the run requires 1`. Runs 65, 72 and 73 are the old USB driver,
run 74 and the volume saved before run 76's flash (run 75's) are PR #466's.

## What the volumes hold

Sector 1080 of each saved partition is the root directory's first (8 reserved
sectors and two 536-sector FATs, one sector per cluster). Bytes 0x60 to 0x7F of
it, as `xxd` prints them from `t14-run76/sda3-before-flash.img`:

```
00000060: 4233 0035 0033 0039 002e 000f 00ac 6c00  B3.5.3.9......l.
00000070: 6f00 6700 0000 ffff ffff 0000 ffff ffff  o.g.............
```

Ordinal `0x42`: the last long-name entry of a two-entry run, checksum `0xAC`,
for a name ending `3539.log`. The ordinal-1 entry and the short entry that
belong after it are not there: on run 75's volume the next 32 bytes are zero,
and on the other four they hold the `ATTEMPTS` entry the loader's next pass
wrote into the first free slot it found.

## How it gets there

`toyos_fat32::dir::insert_entry` writes a long-name run one 32-byte entry at a
time, each through `write_entry_at`, and so each as its own block write. The
first WRITE(10) a boot issues is `logd` creating its file, which is the write
the arm abandons: `usb-storage: write of 1 blocks at 11399 failed on disk 0`,
and 11399 host blocks of 4096 B is sector 91192, the log partition's 90112 plus
1080. The driver answered that write with an error and the stick had taken it.
A write a transport broke in has an outcome nobody knows; this one reached the
flash in five boots of five.

`insert_entry` then calls `erase_inserted(dir_start, start, written)` with
`written == 0`, because `written` counts the writes that returned `Ok`. It
erases nothing, and the entry whose write failed is the one entry that may be on
the medium.

## What is not known

Whether the entry reached the flash from the abandoned command or from the one
the driver re-issued after its recovery; both carried the same bytes. No record
says, and it does not change the defect: either way the volume holds an entry
the filesystem believes it never wrote.

## Exit condition

A create whose first long-name write is refused by the device, with the device
still answering afterwards, leaves a directory `toyos-fat32-check` passes: the
erase covers the entry whose write failed as well as the ones before it. Judged
under QEMU by a block device that commits one write and answers it with an
error, and on the T14 by the fat check of a `usb-transport-break` boot.
