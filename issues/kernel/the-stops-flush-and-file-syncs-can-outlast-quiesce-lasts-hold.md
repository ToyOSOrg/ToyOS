---
status: open
kind: defect
opened: 2026-09-27
---

# The stop's flush and file syncs can outlast `quiesce-last`'s hold

init sequences a stop as logd's flush, bounded by `FLUSH_BOUND` (5 s), then a
sync of every writable file server's volume, each bounded by the same 5 s
(`userland/init/src/main.rs`, `Init::stop`, `sync_files`), and only then asks
the kernel. `quiesce-last-exit` and `quiesce-last-park` hold their thread for
`STAGED` (10 s, `kernel/src/quiesce.rs`) waiting for the stop to come down to
it alone, and panic past it. The two bounds are not held to each other: a
stop that spends its whole flush bound and part of the syncs' reaches the
kernel after the hold has given up.

Seen on `quiesce_wakes_on_the_last_exit`, red wide and green alone, one
named test on a loaded dev host: logd's write to `/log` took 5.016 s, init
said logd did not answer the flush in 5000 ms, and the kernel panicked at
10.499 s naming the stop that never came down to the held thread.

**Exit**: the staging's bound is derived from the stop's own worst case — the
flush bound plus every sync's — or the stop's bounds are one budget the
staging reads, with the wide run green on a loaded host.
