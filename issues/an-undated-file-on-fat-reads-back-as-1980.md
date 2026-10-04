---
status: open
kind: defect
opened: 2026-09-28
---

# An undated file on FAT reads back as 1980

A file written on a machine whose RTC never answered is undated, an mtime of 0
(`toyos_abi::syscall::Stat::mtime`). FAT has no undated stamp:
`toyos-fat32`'s `FatTime::from_unix_secs` clamps 0 up to `FatTime::EPOCH`,
1980-01-01, and the mount answers that back as a date. So the file reads as
written in 1980, which std reports as an instant and not as undated.

**Mechanism read off the code; not reproduced.**

**Exit condition.** A FAT mount answers 0 for an entry stamped
`FatTime::EPOCH`, with a guest test on the `rtc-dead` machine that writes a
FAT file and finds it undated.
