---
status: open
kind: tooling
opened: 2026-09-29
---

# Two shared members assume a bcachefs `/home`, and the T14 has no data partition

`fs_large_file` asserts a 4096-byte name is refused because "no btree value
can hold" it, and `home_backing_revoked` asserts a read through a deleted
file's descriptor is refused because `/home`'s `NvmeBacking` revokes freed
blocks. Both premises are bcachefs's. The T14 carries no `TOYOS-DATA` partition, so `/home` is a tmpfs, which has no name
bound (`kernel/src/tmpfs.rs`) and keeps an unlinked open file's bytes.

## Measured

Every one of the 32 storage lines in that log says 0 TOYOS-DATA partitions and a
tmpfs, the `shared` boot's included: both tests are red on the T14, always.

The full T14 run of `main` at `7e151819`
(EXIT=1), boot `shared`:

```
storage: this machine carries 0 TOYOS-DATA partitions, and a data volume is one
storage: /apps, /config, /home and /state are a tmpfs — they will not survive a reboot
thread 'main' (1) panicked at src/bin/fs_large_file.rs:56:5:
rename accepted a 4096-byte name
[... cpu0] exit: test_rs_fs_large_file pid=118 code=101 cpu=7ms
thread 'main' (1) panicked at src/bin/home_backing_revoked.rs:93:17:
byte 0 read through the deleted file's descriptor is 0xa7, not zero — the backing still resolves blocks the allocator has taken back
[... cpu7] exit: test_rs_home_backing_revoke pid=136 code=101 cpu=11ms
```

`0xa7` is the victim's own byte, not the attacker's `0x5c`.

## Exit condition

Both tests state the filesystem they judge and are staged only on a boot
whose `/home` is that filesystem, and two consecutive T14 runs exit both 0; then this file is deleted.
