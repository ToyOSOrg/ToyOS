---
status: expected-red
kind: tooling
opened: 2026-09-29
---

# Two shared members assume a bcachefs `/home`, and the T14 boot that redded them had a tmpfs one

`fs_large_file` asserts a 4096-byte name is refused because "no btree value
can hold" it, and `home_backing_revoked` asserts a read through a deleted
file's descriptor is refused because `/home`'s `NvmeBacking` revokes freed
blocks. Both premises are bcachefs's. The `shared` boot that redded them
found no `TOYOS-DATA` partition and put `/home` on tmpfs, which has no name
bound (`kernel/src/tmpfs.rs`) and keeps an unlinked open file's bytes.

## Measured

The full T14 run of `main` at `7e151819`
(`/Users/jan/.claude/jobs/2280e09e/tmp/scratchpad/orch/main-metal-full.log`,
EXIT=1), boot `shared`:

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

**Flaky, not stable:** the orchestrator reports both names green on an earlier
T14 run of the same `main`, whose log was lost; what `/home` was on that boot
is not known.

## Exit condition

Both tests state the filesystem they judge and are staged only on a boot
whose `/home` is that filesystem, and two consecutive T14 runs exit both 0;
then their rows in `src/redlist.rs` and this file are deleted.
