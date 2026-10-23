---
status: open
kind: defect
opened: 2026-10-23
---

# A listing the kernel builds for one call is charged to nobody

What a call allocates and frees before it returns is not charged: it draws on
the superframes the kernel keeps (`kernel/pure/frames.rs`), and the kernel
heap panics if those run out while a process holds every other frame. That
holds only while what calls hold at once stays inside them. Most of it is
bounded by the thread that holds it, which is charged for one call's copies
(`loader::start::KernelStack::heap`: two strings at `MAX_USER_STR` and a page),
and a spawn charges what it copies past that.

**`SYS_READDIR`'s listing is not.** `Vfs::list` builds a `Vec<(String, u64)>`
of up to `MAX_LIST_ENTRIES` (16,384) names, each up to `MAX_PATH` (4,096)
bytes, and `syscall::fs::sys_readdir` holds it while it copies it out, with
preemption on. Derived from the bounds, not measured: one listing of a `/tmp`
directory filled with long names is up to 64 MiB, so a few threads listing at
once past what the kernel keeps, with every other frame committed, panic the
kernel. Each name is charged where tmpfs keeps it, so making the directory is
refused under exhaustion; listing one made before is not.

`SYS_SYSINFO`'s roster (`MAX_SYSINFO_THREADS` records) is built under the
process table's lock, with preemption off, so one CPU holds one at a time.

**Exit:** a listing charged as it is built, or written out a bounded piece at
a time, so that no call holds a heap allocation sized by a directory; and a
guest case in `kernel_objects_refused` that lists a directory of long names
with every frame taken.
