---
status: open
kind: finding
opened: 2026-08-23
---

# A 1.6 MB file written to /home was spawned as "fewer bytes than a file header"

Seen once, on the dev host, in a loaded parallel suite run of 2026-08-23.
`disk_backtrace` copies its child binary to `/home` and spawns it. Its own
stdout says the copy happened; the kernel says the spawn found nothing:

```
  copied 1659240 bytes to /home/disk_backtrace/child
  spawn /home/disk_backtrace/child: other error
[kernel 5.895 cpu1] spawn: /bin/test_rs_disk_backtrace pid=67 ...
[kernel 5.950 cpu1] spawn: /home/disk_backtrace/child: ELF: fewer bytes than a file header
```

Fifty-five milliseconds between the two lines. The test is green alone (77 ms)
and was green in three later suite runs on the same host, and a run of the same
suite on the base commit did not reproduce it — so it is a race, and what makes
it worth a file rather than a shrug is that the symptom is specific: a path that
resolved, opened, and then measured at less than 64 bytes.

The write-back queue landed the same day (#257), and `tests/CLAUDE.md` already
records that a file's last close no longer flushes or drops synchronously.
Whether that is this — the queue leaves the *pages* cached, so a reopen should
still see them, and a size or directory entry that has not caught up is a
different mechanism from stale data — is exactly what has not been established.
Nobody should re-derive the sighting; somebody should reproduce it under a copy
loop and say which of the two it is.

The reader that matters is `sys_spawn`'s ELF header read, not `disk_backtrace`:
a program that copies a file and runs it is an ordinary thing to do, and if the
size a spawn reads can trail the bytes a close wrote, the test is only where it
was noticed.
