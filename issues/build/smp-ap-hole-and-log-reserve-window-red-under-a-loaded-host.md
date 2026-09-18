---
status: open
kind: tooling
opened: 2026-09-16
---

# `smp_failed_ap_leaves_no_hole` and `log_reserve_window` red under a loaded host

Both `Sched::Parallel`, both first sightings, both in one `cargo test` fast-tier
run on `wt/toyos-usbhang` sharing the host's twelve guest slots throughout with
two other worktrees' suites (`[host-slots]` names `pid 49668` and `pid 2726`
holding slots across the whole run). The branch's own diff at the time —
`tests/toyos.rs`'s tier table, `src/tiers.rs`'s `RELEGATED` and
`tests/test-durations`, all for `usb_reset_records_the_phase_it_cut` — touches
neither SMP bring-up, `spawn_init`, nor `rootfs.rs`, so it is filed rather than
chased. Same shape as
`issues/build/parallel-tests-red-under-other-suites.md`, filed separately
because that file is not this agent's to append to.

`smp_failed_ap_leaves_no_hole` FAILED at `[kernel 2.605 cpu0] PANIC: panicked
at src/loader/mod.rs:928:50: spawn_init: failed to spawn: WouldBlock` — a
`WouldBlock` surfacing from a call the boot path expects to complete, this
sibling file's canonical shape for a starved host (`fs_transactional`'s
`cleanup: Kind(WouldBlock)` is its precedent). `ALONE: GREEN`,
`cargo test -- smp_failed_ap_leaves_no_hole` alone immediately after, PASS in 2s.

`log_reserve_window` FAILED at `[kernel 2.483 cpu0] PANIC: panicked at
src/rootfs.rs:71:9: boot: root=5481a0b70d73284f6ab07dd84e083efa matches 0 of
the 0 TOYOS-ROOT partition(s) this machine carries`, on both this boot and its
retry. The cause is upstream and in the same capture: `usb-storage: 00:01.0
slot 1 transport broke on SCSI 0x28: no answer in the data phase in 2000 ms` —
the root disk's own read missed its 2000 ms budget under the host's load, so
the GPT scan that follows legitimately found no partition to mount. `ALONE:
GREEN`, `cargo test -- log_reserve_window` alone immediately after, PASS in 3s.

Neither name is in `src/redlist.rs` yet. Not investigated further.
