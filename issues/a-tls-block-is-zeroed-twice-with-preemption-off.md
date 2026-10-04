---
status: open
kind: defect
opened: 2026-10-03
---

# A TLS block is zeroed twice with preemption off

`kernel/src/loader/tls.rs`'s `build_combined` takes its frames from
`PageAlloc::new`, which is `pmm::alloc_contiguous`, which zeroes every 2 MiB
frame it hands out; then it zeroes the whole block again with `write_bytes`.
Every `SYS_THREAD_SPAWN` (`process::spawn_thread`) and every `SYS_SPAWN`
(`loader`) builds one, and a syscall runs with preemption off from entry to
exit (`issues/syscall-preemption-is-incidental.md`), so each spawn pays
two writes of the block, at least 2 MiB each, in one preemption-off window.

By reading, not measured.

Owner: `issues/toyos-beats-linuxs-latency-on-the-t14.md`.

**Exit**: `build_combined` zeroes no byte the PMM already zeroed.
