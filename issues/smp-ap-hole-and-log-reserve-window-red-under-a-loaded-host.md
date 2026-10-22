---
status: open
kind: defect
opened: 2026-09-16
---

# The first spawn answered `WouldBlock` under a loaded host

`smp_failed_ap_leaves_no_hole` red once in a `cargo test` fast tier on
`wt/toyos-usbhang`, sharing the host's twelve guest slots throughout with two
other worktrees' suites (`[host-slots]` named `pid 49668` and `pid 2726`
holding slots across the run). The branch touched neither SMP bring-up nor the
first spawn:

`[kernel 2.605 cpu0] PANIC: panicked at src/loader/mod.rs:928:50: spawn_init:
failed to spawn: WouldBlock`, a `WouldBlock` from a call the boot path expects
to complete (`fs_transactional`'s `cleanup: Kind(WouldBlock)` was its
precedent). `ALONE: GREEN`, alone immediately after, in 2 s.

The first spawn is `spawn_supervisor` now (`kernel/src/loader/mod.rs`), and it
still panics on any refusal. What answered `WouldBlock` was never found.

The same run red `log_reserve_window` on a ROOT read that missed its 2000 ms
budget in the kernel's usb-storage driver. That half is gone: the kernel reads
ROOT only from the image the loader read into memory (`kernel/src/rootfs.rs`)
and drives no USB (`13ed7b4f4`).

**Both tests are deleted**, as flaky tests are: `aedcf17dc` took
`smp_failed_ap_leaves_no_hole` out with `smp_hole_shootdown`, and `4db54ffa5`
and `3b8102cf5` `log_reserve_window` with what it rode.

**Exit**: a cause for the first spawn's `WouldBlock`, fixed there, or the
first spawn shown unable to answer it.
