---
status: open
kind: defect
opened: 2026-09-07
---

# One VFS lock serialises every write, and on a real stick it costs 22 s for 2.4 MiB

`kernel/src/vfs.rs:32`'s `lock()` is one machine-wide ticket spinlock over the
whole VFS, taken for the length of every operation that touches it. Under QEMU
nothing shows: the emulated disk answers in microseconds and the lock is never
held long enough to contend. On the T14 it is the boot's dominant cost.

## What the machine said

T14 run 20, `tests/metaldevicecase`, branch tip `f46f91eb`
(`/Users/jan/.claude/jobs/2280e09e/tmp/t14-run20/metaldevicecase.log`):

| | |
|---|---|
| what `usbwrite` moved | 2,457,600 B — `userland/metalprobe/src/usb.rs`'s `BYTES`, which is `SLOWEST_KIB_S 512 × JOB_BOUND_MS 60_000 × SHARE_PERCENT 8 / 100 / 1_000 × 1024` |
| what it took | `exit: usbwrite pid=7 code=22091910 cpu=32100ms` — 22.09 s of span, 32.1 s of CPU |
| what it spent in the kernel | `syscalls: pid=7 total=52 syscall_wall=31957ms` — 52 syscalls, 31.96 s of wall inside them |
| the rate that implies | 108.6 KiB/s, against the 512 KiB/s `SLOWEST_KIB_S` calls "an order of magnitude under any USB 2.0 flash device" |
| the interrupts it cost | `irq: cpu0 total=103171 … xhci=103161` for that one job |
| the device's own flushes | `flush-census: dev=16 flushes=9 … p50<=512us p99<=1024us max=630us of 9 flushes` |
| the stick | `usb-storage: disk 0 ready on slot 5, 7507812 blocks of 512 B (29327 MiB)` — a SanDisk Ultra |

And beside it, twelve times in the same boot:

```
[08:51:09 15.024 cpu4] LOCK CONTENTION: 50M spins at src/vfs.rs:32:18, ticket=38 now=37
[08:51:12 18.745 cpu4] LOCK CONTENTION: 100M spins at src/vfs.rs:32:18, ticket=38 now=37
[08:51:22 28.192 cpu3] LOCK CONTENTION: 50M spins at src/vfs.rs:32:18, ticket=41 now=40
[08:51:22 28.522 cpu4] LOCK CONTENTION: 50M spins at src/vfs.rs:32:18, ticket=42 now=40
```

`ticket=42 now=40` is two waiters queued behind one holder. Each 50M-spin
report is about three and a half seconds of one CPU spinning with preemption
off, and the tripwire panics the machine at 500M — so this boot ran at a fifth
of the way to a `DEADLOCK` panic, repeatedly, on a healthy device.

The `flush-census` line is what rules the device out: nine cache flushes,
630 µs at worst. The stick is not what is slow.

## What it costs, beyond the rate

`tests/metal-profile.toml` prices `usbwrite.metaldevicecase.span_us` at
4,800,000 and the machine answered 22,091,910 — the first T14 reading of that
row is four and a half times its ceiling. And the cost is not only that row's:
one bound covers the whole job list, so a first job that spends 22 s of 60 is
why run 20's list never reached its `reboot` job and was ended by the runner's
deadline instead. Two more jobs at that rate and the reset lands wherever the
list happens to be, which is the case
`issues/kernel/quiesce-runs-while-userland-still-does-io.md` is about.

## What would show it

The same boot with the VFS lock's hold narrowed to the metadata it guards
rather than the transfer it does not, measured against the same
`exit: usbwrite … code=` number on the same machine — the span is already the
job's own exit code, so the instrument exists. `LOCK CONTENTION` lines going to
zero on that boot is the second reading, and it is the one that says the cause
was the lock rather than the bus.
