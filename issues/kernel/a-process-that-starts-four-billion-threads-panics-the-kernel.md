---
status: open
kind: defect
opened: 2026-10-01
---

# A process that starts four billion threads panics the kernel

`process::spawn_thread` takes a thread's id from its process's
`IdMap<Tid, ThreadEntry>` (`kernel/src/id_map.rs`), whose `insert` steps its
counter with `Tid + Tid` (`toyos-abi/src/lib.rs`). The kernel builds with
`overflow-checks = true` (`kernel/Cargo.toml`), so the 2^32nd thread a single
process starts panics the kernel, and it does so under `PROCESS_TABLE`, which
hangs the machine instead of reporting. The thread before it is issued
`Tid(u32::MAX)`, which is the per-CPU word for no thread
(`arch::x86_64::percpu::current_tid`), so `handle_page_fault` refuses every
fault that thread takes.

Userland reaches it by starting and joining threads in a loop; only a
successful start spends an id, so it costs 2^32 thread starts, never measured.
A pid had the same shape until `kernel::proclife::pids` (stops below `Pid::MAX`
and refuses the next spawn by name); a thread id needs the same: never
`Tid::MAX`, and a refused start once the ids are spent.

*Exit*: a host test of the thread-id allocator answering a refusal after
`Tid(u32::MAX - 1)`, and `SYS_THREAD_SPAWN` answering it as a refusal.
