---
status: open
kind: defect
opened: 2026-08-29
---

# The GPT flooring concession is a parser constant where the floor is the caller's fact

`parse_header`'s backup bound concedes `MAX_LBA_BYTES / lba_bytes - 1` LBAs
because the kernel's `DeviceSectors` adapts a 4 KiB `BlockDevice` and reports
`lba_count` floored to whole caller blocks. But the floor belongs to the
caller, not the parser: a host tool reading a file byte-exactly, or a 4Kn
device, floors nothing — yet every 512-byte view pays the full 7-LBA
concession, so a table reaching up to 7 LBAs into the mirror array's low end
parses for every caller because one caller is coarse.

The real answer is the `Sectors` contract (or the block layer beneath the
kernel's implementation of it) reporting the caller's true block granularity
or byte capacity, so an exact view concedes 0 and the kernel's view concedes
exactly its own remainder — the backup bound then becomes exact again for
everyone. The interim in `parse_header` is the fixed slack plus the clamp
(`backup_array_lba` never past `lba_count - 1`, whatever `entry_count`
claims), which keeps the backup header unconcedable but leaves the array's
low-end sliver open at every coarse-view width.
