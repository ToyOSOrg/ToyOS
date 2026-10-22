---
status: open
kind: defect
opened: 2026-10-03
---

# A TLS block is zeroed twice with preemption off

`kernel/src/loader/tls.rs` lays a block out in a zeroed heap buffer, then
copies it into an object whose frames `pmm::alloc_frame` zeroed
(`SharedMemObject::create`): every byte of the block is written three times.
Every `SYS_THREAD_SPAWN` (`process::spawn_thread`) and every `SYS_SPAWN`
(`loader`) builds one, and a syscall runs with preemption off from entry to
exit (`issues/syscall-preemption-is-incidental.md`). The block is whole pages
of its layout, a page for most programs, not the 2 MiB it once was.

By reading, not measured.

Owner: `issues/toyos-beats-linuxs-latency-on-the-t14.md`.

**Exit**: `build_combined` zeroes no byte the PMM already zeroed.
