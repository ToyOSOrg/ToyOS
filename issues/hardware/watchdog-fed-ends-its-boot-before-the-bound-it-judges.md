---
status: open
kind: tooling
opened: 2026-09-30
---

# `watchdog_fed` ends its boot before the bound it judges

`watchdog_fed` holds that an armed T14 boot runs its list to its own stop,
which a chipset reset anywhere in it would have cut short
(`tests/toyos.rs:1740-1749`). Its boot, `testcases-watchdog`, carries no job of
its own and neither does `loader_watchdog_arms`' arm on it
(`tests/toyos.rs:1667-1679`), so its list is `reboot` alone: on two T14 runs
the runner spawned `reboot` 1.196 s and 1.489 s into the kernel's clock, and the
shutdown disarms the timer (`kernel/src/syscall/machine.rs:88`), inside one
`toyos_tco::BOUND_MS` of 9,600 ms. A kernel that never fed the timer passes it.

**Exit**: `watchdog_fed`'s boot holds past `toyos_tco::BOUND_MS` before its
`reboot`, and the same boot armed with `tco-starve` reds it once stage 1 of
`issues/hardware/a-frozen-toyos-waits-for-a-hand-on-the-power-button.md` has
seen the TCO reset the T14.
