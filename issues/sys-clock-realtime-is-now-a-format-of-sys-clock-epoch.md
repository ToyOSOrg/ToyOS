---
status: open
kind: defect
opened: 2026-09-28
---

# `SYS_CLOCK_REALTIME` is now a format of `SYS_CLOCK_EPOCH`, and retiring it is an ABI discussion

Since the RTC-keeps-UTC ruling, `SYS_CLOCK_REALTIME`'s handler and
`SYS_CLOCK_EPOCH`'s both read `crate::clock::utc_secs()`; the first packs it
into `h:m:s` (`toyos_abi::syscall::clock_realtime`), which every caller today
could derive from `clock_epoch()` instead, with no information
`SYS_CLOCK_REALTIME` carries that `SYS_CLOCK_EPOCH` does not.

Exit condition: `SYS_CLOCK_REALTIME`, its kernel handler and `toyos_abi::syscall::clock_realtime` are
deleted, `toyos::system::clock_realtime` and both its callers
(`userland/compositor/src/render.rs`,
`tests/toyos-rust-tests/src/bin/wall_clock_now.rs`) move to `clock_epoch`, all
in a PR of their own, and this file is deleted in that PR's merge.
