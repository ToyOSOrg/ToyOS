---
status: open
kind: defect
opened: 2026-09-07
---

# One VFS lock is held across a USB write, and 32 s of it comes within 2x of the deadlock panic

T14 run 20, `tests/metaldevicecase`, `metal-suite` f46f91eb. A 2.4 MiB write to
the boot stick took 32 s, and for its whole length other CPUs sat on
`vfs::lock()`:

```
[08:51:38 44.923 cpu4] LOCK CONTENTION: 50M spins at src/vfs.rs:32:18, ticket=57 now=56
[08:51:42 48.634 cpu4] LOCK CONTENTION: 100M spins at src/vfs.rs:32:18, ticket=57 now=56
[08:51:50 56.046 cpu4] LOCK CONTENTION: 200M spins at src/vfs.rs:32:18, ticket=57 now=56
```

`VFS` is one `Lock<Option<Vfs>>` for the whole machine (`kernel/src/vfs.rs:14`),
and `ops::fsync` (`kernel/src/object/ops.rs`) holds it across `flush_file` **and**
`sync_for_path` together — deliberately, so a volume cannot be unmounted between
them — which means across every device write and the SYNCHRONIZE CACHE under
them. `logd` fsyncs after every batch, on the same stick.

**The number that matters is how close this is to a panic.** From the log's own
timestamps, 50M spins is 3.72 s (15.024 → 18.745 s), so `Lock::lock`'s
`DEADLOCK` threshold of 500M spins (`kernel/src/sync.rs`) is about **37 s of
continuous contention**. This boot reached 200M. A write 1.8x slower — a bigger
file, a slower stick, a device that retries — panics the machine inside a lock
acquisition, and the report goes to a `logd` that is on the far side of the lock
it is panicking about. That is a machine that cannot say why it died.

**It is not the wedge in
`issues/hardware/a-t14-boot-wedges-after-a-jobs-exit-and-nothing-said-why.md`,
and that was checked rather than assumed.** A spawn does take `vfs::lock()` —
`open_backing` at `kernel/src/loader/mod.rs:370` and twice in `load_needed_libs`
— but all three are *before* the `ELF: … relocations indexed` and
`spawn: TLS … modules` records, which every hung boot wrote. What runs after
them reads the already-open backing directly (`read_file_range`,
`elf::read_backing_into`) and takes no VFS lock at all. So the contention is
upstream of the window the hangs stop in.

**What rules the device out, and what it costs beside the panic distance.** The
payload is 2,457,600 B — `userland/metalprobe/src/usb.rs`'s `BYTES`, which is
`SLOWEST_KIB_S 512 x JOB_BOUND_MS 60_000 x SHARE_PERCENT 8 / 100 / 1_000 x 1024`
— and `exit: usbwrite pid=7 code=22091910 cpu=32100ms` makes that 108.6 KiB/s,
a fifth of the 512 KiB/s floor the same file calls "an order of magnitude under
any USB 2.0 flash device". The stick is not what is slow: `flush-census: dev=16
flushes=9 ... p50<=512us p99<=1024us max=630us of 9 flushes`, and
`syscalls: pid=7 total=52 syscall_wall=31957ms` puts the whole cost inside 52
syscalls. Two rows pay for it: `usbwrite.metaldevicecase.span_us` reads
22,091,910 against its committed ceiling of 4,800,000, and one bound covers the
whole job list — which is why run 20's list never reached its `reboot` job and
was ended by the runner's deadline instead.

**Exit condition**: the sync half of `fsync` off the global VFS lock, or a bound
on how long that lock may be held that is under `Lock::lock`'s tripwire rather
than within 2x of it.
