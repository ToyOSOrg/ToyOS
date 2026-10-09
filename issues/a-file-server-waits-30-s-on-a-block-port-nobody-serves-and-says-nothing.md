---
status: open
kind: defect
opened: 2026-10-09
---

# A file server waits 30 s on a block port nobody serves, and says nothing

An image whose `[boot]` `start` runs `fileserver` and no `diskserver`, on a
machine whose every partition is on a disk the kernel does not drive, stands
still for about thirty seconds after its three file servers are spawned, and
no line of the boot says who is waiting or on what.

Measured once, on `nvme_disk_keeps_log_and_home`'s machine with `diskserver`
taken out of `tests/testcases/system.toml`'s `start` (host load 52 by
`uptime`'s one-minute mean); the guest's own stamps:

```
[10.330 cpu0 kernel] spawn: /system/bin/fileserver pid=2 …
[10.362 cpu0 kernel] spawn: /system/bin/fileserver pid=3 …
[10.377 cpu0 kernel] spawn: /system/bin/fileserver pid=4 …
[40.513 cpu0 kernel] spawn: /system/bin/logkeeper pid=5 …
[40.526 cpu0 kernel] spawn: /system/bin/soundserver pid=6 …
```

No `fileserver:`, `supervisor:` or kernel line falls between the third and
the fourth, and none after it names a wait that ran out. The test went red
only on its own ceiling, `[qemu] Boot timed out waiting for ===READY===`.

Not read: which call waits, and whose bound the thirty seconds are.
`fileserver` reaches the port through `diskserver::list` and
`diskserver::Session::open` on `toyos_blockring::PORT`
(`userland/fileserver/src/main.rs`), and the supervisor starts `logkeeper`
only after the file servers.

Where `diskserver` runs and is refused its controller the same servers answer
at once and by name (`fileserver: Log serving /log — absent: …`), so the wait
is the unserved port's alone.

**Exit condition.** On that boot the file servers say, by role, that no block
service answers, and the boot reaches its ready marker without the wait: a
guest test of an image with `fileserver` and no `diskserver` on a
`Storage::Disk` profile requires the three lines and reds on the gap.

## Owner

`userland/fileserver` and `userland/supervisor`; unheld.
