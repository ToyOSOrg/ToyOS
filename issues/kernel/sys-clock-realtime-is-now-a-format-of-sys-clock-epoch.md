---
status: owner
kind: question
opened: 2026-09-28
---

# `SYS_CLOCK_REALTIME` is now a format of `SYS_CLOCK_EPOCH`, and retiring it is an ABI discussion

Since the RTC-keeps-UTC ruling, `SYS_CLOCK_REALTIME`'s handler
(`kernel/src/syscall/dispatch.rs:271`) and `SYS_CLOCK_EPOCH`'s both read
`crate::clock::utc_secs()`; the first packs it into `h:m:s`
(`toyos_abi::syscall::clock_realtime`, `toyos-abi/src/syscall.rs:1104`), which
every caller today (`userland/compositor/src/render.rs:262`,
`tests/toyos-rust-tests/src/bin/wall_clock_now.rs:22`) could derive from
`clock_epoch()` (`:1119`) instead, with no information `SYS_CLOCK_REALTIME`
carries that `SYS_CLOCK_EPOCH` does not.

Owner: whoever takes the ABI discussion the root `CLAUDE.md` requires before a
syscall is changed or removed — nobody may retire it unilaterally. Exit
condition: `SYS_CLOCK_REALTIME`'s number (42) is retired and never reused, its
handler, wrapper and the one caller each are deleted in a PR of their own, and
this file is deleted in that PR's merge.
