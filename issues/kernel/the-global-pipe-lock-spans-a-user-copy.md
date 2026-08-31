---
status: open
kind: defect
opened: 2026-08-28
---

# `pipe::try_read`/`try_write` hold the one global `PIPES` spinlock across a 2 MiB user copy, and a pipe's first write adds a bitmap scan and a 2 MiB zeroing under it

Every pipe and every IPC `Connection` in the machine lives behind one static: `static PIPES: Lock<Option<IdMap<PipeId, Pipe>>>` (`kernel/src/pipe.rs:174`). `with_pipes_mut` takes it and runs the whole closure under it (`pipe.rs:181-184`), and `sync::Lock::lock` calls `crate::preempt::disable()` before it spins (`kernel/src/sync.rs:56`), so the holder is non-preemptible and every other CPU that wants a pipe spins with preemption off too.

## The mechanism

The bulk copy is inside that closure, not outside it.

* `pipe::try_read` (`pipe.rs:210`) calls `ring.read(len, |off, src| buf.write_at(off, src))` at `pipe.rs:220`, inside `with_pipes_mut`.
* `pipe::try_write` (`pipe.rs:244`) calls `backing.ring.write(buf.len(), |off, dst| buf.read_at(off, dst))` at `pipe.rs:254`, same acquisition.
* `Ring::read`/`Ring::write` (`toyos-abi/src/ring.rs:141`, `:178`) hand the closure one or two contiguous runs; the closure is `UserBytesMut::write_at` / `UserBytes::read_at` (`kernel/src/user_ptr.rs:206`, `:164`), a `copy_nonoverlapping` of the whole run.

The size is bounded only by the ring: `PIPE_SIZE = PAGE_2M` (`pipe.rs:104`), `PAGE_2M = 2 * 1024 * 1024` (`toyos-userbound/src/span.rs:27`), and `capacity = total_size - size_of::<RingHeader>()` (`ring.rs:60`) with `RingHeader` `#[repr(C, align(64))]` holding one `AtomicU32` (`ring.rs:24-27`) — so **2,097,088 bytes** is the largest single copy under the lock. Nothing above caps it: `SYS_READ`/`SYS_WRITE` pass the userland length straight through (`kernel/src/arch/syscall/dispatch.rs:110-116`), `object::ops::try_read` hands the full window to `pipe::try_read` (`kernel/src/object/ops.rs:339-340`), and the only other bound, `user_ptr::window` (`user_ptr.rs:269`), requires physical contiguity — which a demand-paged 2 MiB frame satisfies exactly.

A pipe's **first** write is worse, because the page is allocated lazily under the same lock. `try_write` calls `pipe.back()` (`pipe.rs:250`), which calls `pmm::alloc_page(pmm::Category::Pipe)` (`pipe.rs:148`). That takes `BITMAP` nested inside `PIPES` (`kernel/src/mm/pmm.rs:221`), linearly scans up to the whole physical bitmap for a free frame (`pmm.rs:224-241`), and then `write_bytes(..., 0, PAGE_2M)` — a 2 MiB zeroing (`pmm.rs:233-237`) — before `Ring::new` and the user copy that follows it.

## What queues behind it

Everything pipe-shaped in the kernel goes through the same static: `create` (`pipe.rs:191`), `map_page` (`pipe.rs:206`), `has_data`/`has_space` (`pipe.rs:261`, `:267`) — which the inbox readiness predicate calls per registered source (`kernel/src/inbox.rs:767-768`) and the blocking read/write wrappers call per attempt (`kernel/src/arch/syscall/io.rs:76`, `:162`) — every `PipeReader`/`PipeWriter` clone and close (`pipe.rs:74-80`, `close_read`), and `add_inbox_watcher`/`remove_inbox_watcher` (`pipe.rs:338`, `:348`). A compositor client, netd, logd and the shell all contend the same word.

## Impact, stated at its real size

This is **not** the device-I/O class. The copy is memory-speed and takes no device round trip, so it does not approach `sync::Lock`'s 500,000,000-spin `DEADLOCK` panic (`sync.rs:72-75`) the way `BLOCK_DEV` and `vfs::VFS` do — that class is tracked in `issues/kernel/every-wait-in-this-kernel-is-a-spin.md` and is not what this file is about.

What it is: a global lock held with preemption disabled for a stretch that scales with a userland-chosen length up to 2 MiB, on the path that carries every IPC message in the system. The scheduler's own bar for an uninterruptible stretch is `MAX_PASS_NS = 200_000` ns (`toyos-sched/src/cpu.rs:1235`); a 2 MiB copy plus, on first write, a 2 MiB zeroing and a bitmap scan is on the wrong side of that at any plausible memory bandwidth. **The hold time is not measured and that is the first thing owed** — `io-depth-probe` and the `audio_tone` worst-wake instrument are the tools that would price it, the same pair `issues/audio/disk-wait-pins-a-cpu.md` used for the device path.

Two things make it worth closing rather than noting. The convoy is machine-wide, not per-pipe: one process reading 2 MiB out of a full pipe stalls an unrelated process's `SYS_CONNECT` and an unrelated inbox's readiness poll. And it is directly userland-driven — the length is the caller's, so a program that wants the stall only has to ask for it.

## Precondition / repro

1. Create a pipe or connection; write until the ring is full (2,097,088 bytes).
2. From the reader, `SYS_READ` with a 2 MiB-aligned buffer inside one demand-paged 2 MiB region and `len` = 2 MiB, so `user_ptr::window` accepts it as one contiguous window.
3. From another CPU, time an unrelated `pipe::has_data` or `pipe::create`; it waits for the whole copy.

For the allocation half, the repro is simpler: `SYS_CONNECT` then a single first `SYS_WRITE` of one byte — `pipe.rs:250` -> `pipe.rs:148` -> `pmm.rs:233-237` runs the scan and the 2 MiB zeroing under `PIPES` before one byte moves.

## Fix direction

`PIPES` is doing two jobs and only one of them needs the global. It is the table lock, and it is also the serialization `Ring` relies on — `ring.rs:41-43` says so plainly: "Every access is serialized by the caller's lock (`kernel::pipe`'s `PIPES`), which is why the cursors are plain integers rather than atomics." So the copy cannot simply be lifted out; the ring's data-race argument has to move with it.

The shape that keeps that argument true is a per-pipe lock under the table lock: `PIPES` guards the `IdMap` and the refcounts, a `Lock` (or a claimed-range token) inside `Pipe` guards `Backing`, and the table lock is dropped once the per-pipe guard is in hand. `try_read`/`try_write` then hold only the pipe's own lock across `Ring::read`/`Ring::write`, and `has_data`/`has_space`/`create`/`clone`/`close` stop queueing behind a stranger's copy. `Ring`'s header sentence changes with it — the serializing lock becomes the pipe's, not the table's.

The lazy allocation should move out from under both: allocate the ring page before taking the table lock (or, on the miss, drop and retry the way `file_cache::read_page` already does at `kernel/src/file_cache.rs:217-231`), so neither the bitmap scan nor the 2 MiB zeroing runs with a global held.

Sequencing note: this touches `toyos-abi/src/ring.rs`'s doc contract — the sentence naming `PIPES` as the serializing lock — so it lands as one commit over `toyos-abi/src` and `kernel/src` together, which `abi_lands_alone` permits. The two closures at `pipe.rs:220` and `pipe.rs:254` now hand `Ring` a `toyos_abi::ring::Src`/`Dst` rather than a slice; a per-pipe lock changes who serializes them, not their shape.

## Two checks

* **Negative control:** revert the whole change onto the base and re-run the contention measurement; the per-CPU wait on an unrelated pipe operation must return to the pre-change value.
* **Independent oracle:** `kernel-loom`'s existing lock models for the new two-level acquisition (table then pipe), which is a different epistemic source from the kernel's own test suite; plus the `audio_tone` worst-wake instrument, which is a recorded real-failure oracle rather than a re-derivation.
