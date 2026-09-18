---
status: open
kind: defect
opened: 2026-09-18
---

# The shutdown's drain counts the write-back queue while `iod` holds an entry

`kernel/src/writeback.rs`'s `drain_retrying` decides how many entries a pass
owes with `QUEUE.lock().len()`, taken under the queue lock alone.
`drain_one` pops under the VFS lock its caller already holds, and re-enqueues
a budget-refused flush under that same lock — so an entry `iod` has popped is
on no queue for as long as `iod`'s attempt lasts, and a second drainer that
counts in that window counts zero, sees nothing owed, and returns.

The second drainer is the shutdown. `quiesce` calls `writeback::drain_all`
and then `sync_all`, `Rebooting.`, `wait_for_durable` and the reset. If
`iod`'s attempt in that window is refused on budget, the file is still owed
after the shutdown's drain has said nothing is, `iod` parks in its backoff,
and the reset lands over dirty pages nothing flushed. An attempt that
succeeds is harmless: `sync_all` takes the VFS lock and so waits for it.

## What has been seen

The count coming back zero with an entry in `iod`'s hands, staged: a boot of
`tests/quiescetwicecase` on a kernel whose `quiesce-drain-refuse` actuator
refused `iod`'s flush of `/log/quiesce-owed.bin` every time. The shutdown
wrote `Syncing filesystems...` at 0.719 s on cpu0 and `Rebooting.` at 0.722 s
with no attempt of its own on that file, while cpu1 wrote `log-volume: write
of quiesce-owed.bin: the device would not answer in the caller's own budget`
at 0.719, 0.721, 0.734, 0.755, 0.798, 0.881, 1.044, 1.367 and 2.010 s — seven
of them under the boot's last word. One boot in the five that actuator's
first form was booted for; the actuator now keeps `iod` from popping at all,
so that boot no longer stages it.

Not seen: the loss itself on a kernel with no actuator, which needs a real
budget expiry on `iod`'s flush inside the window. A USB stick slow enough to
expire `block::OPERATION` is the machine that would produce one.

## What would show it

An actuator that refuses `iod`'s drain flush once, on budget, while a
shutdown is in its first stage, judged by `toyos-fat32-check` and a byte
comparison of the file on the image after the reset.

**Exit condition**: the shutdown's drain cannot return while a flush is owed
to anybody — the count and the pop under one lock, or the drain waiting on
what `iod` holds — and the boot above reads the file back whole.
