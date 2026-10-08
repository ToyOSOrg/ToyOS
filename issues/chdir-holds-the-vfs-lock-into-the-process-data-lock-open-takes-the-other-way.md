---
status: open
kind: defect
opened: 2026-10-08
---

# A successful chdir reverses the open path's VFS/process-data lock order

`sys_chdir` (`kernel/src/syscall/fs.rs:84`) is written as

```rust
match vfs::lock().cd(&cwd, path) {
    Ok(new_cwd) => { process::with_process_data(|d| d.cwd = new_cwd); 0 }
    Err(e) => e.to_u64(),
}
```

The `vfs::lock()` guard is a temporary in the match scrutinee. Under the kernel
crate's edition (2021, `kernel/Cargo.toml`), a scrutinee temporary lives to the
end of the whole `match`, so the VFS lock is still held through the `Ok` arm
while `with_process_data` takes the process-data lock
(`kernel/src/process.rs:853`). Order: **VFS then process-data.**

`sys_open` (`kernel/src/syscall/fs.rs:35`) takes process-data via
`with_process_data` and calls `ops::open`, which takes `crate::vfs::lock()`
inside that closure (`kernel/src/object/ops.rs`). Order: **process-data then
VFS.**

Two threads of one process (process-data is a per-process `Arc` shared by its
threads; VFS is global) running `SYS_CHDIR` and `SYS_OPEN` on two CPUs can each
hold the lock the other waits for. The ticket spinlock does not break the cycle;
its spin ceiling panics the kernel and unrelated VFS work stalls behind it. This
is a distinct inversion from the one recorded in
`issues/a-user-copy-demand-pages-under-whatever-its-caller-holds.md`.

**Traced, not executed.** No model compiles these two syscalls, and staging the
window needs an actuator the kernel does not have: one that holds `sys_chdir`
in its `Ok` arm until a sibling releases it.

**Exit**: `SYS_CHDIR` and `SYS_OPEN` can run concurrently in one process with no
lock-order inversion — `chdir` does not hold the VFS guard while it takes
process-data (the guard is dropped before the `Ok` arm takes the second lock).
A test that stages chdir past a successful lookup with its guard still alive,
then drives open to VFS acquisition under process-data, completes both without
deadlock.

**Owner**: `sys_chdir`, `kernel/src/syscall/fs.rs`.
