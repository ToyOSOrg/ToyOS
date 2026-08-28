---
status: open
kind: defect
opened: 2026-08-28
---

# A `UserBytes` window outlives the park in `sys_read`/`sys_write`, so a sibling's `munmap` hands the kernel a reissued frame to copy into

`kernel/src/user_ptr.rs`'s module header says a bulk buffer is "a `UserBytes`/`UserBytesMut` window the kernel reads or writes but never borrows". True of the borrow. Not true of the *frame*: the window is a raw direct-map pointer plus a length, and nothing keeps the physical page it names alive for as long as the window is held.

**What the window actually is.** `SyscallContext::user_bytes`/`user_bytes_mut` (`kernel/src/user_ptr.rs:105-116`) call `window` (`:269-288`) once and store `UserBytes { kptr, len, _scope: PhantomData }`. `window` → `translate` (`:74-76`) → `translate_user` (`:62-72`) takes `pt.lock()` transiently per page and drops it before returning; the result is `AddressSpace::translate`'s `DirectMap` (`kernel/src/mm/paging.rs:582,586`), i.e. `phys + PHYS_OFFSET` (`kernel/src/mm/mod.rs:127-128`) — the kernel's permanent alias for that frame. The `PhantomData<&'a>` bounds the *Rust* scope of the syscall and nothing about the frame. There is no pin, no refcount, no quarantine: `grep -rni -e '\bpinned\b' -e refcount -e in_flight -e quarantine kernel/src/` finds writeback, the file cache and the i8042, and nothing here.

**Where it is built, and where it is used.** `kernel/src/arch/syscall/dispatch.rs:110-117` builds the window **once**, before the handler runs — `SYS_WRITE` at `:111`, `SYS_READ` at `:115`. `kernel/src/arch/syscall/io.rs`'s own header states the same shape: "dispatch converts the caller's pointer and length into a window before this module runs." `sys_read` (`io.rs:123-218`) then loops around that one `&mut UserBytesMut`, re-taking `with_process_data` per attempt but dropping it before parking. Every park arm — pipe (`io.rs:154-167`), virtio-sound (`:169-183`), HDA (`:184-198`), keyboard (`:199-213`) — calls `completion::wait_until` (`kernel/src/completion/mod.rs:230-258`), which context-switches away and, on the wake, returns into the same loop with the same stale `kptr`. `sys_write` (`io.rs:47-89`) is the same with `Deadline::never()` at `:70-81`.

**What removes the frame under it.** `sys_munmap` (`kernel/src/arch/syscall/vm.rs:156-174`) matches the region by base address alone (`:159`), `swap_remove`s it and `free_and_unmap`s it (`:160-166`), then drops the `Unmapped` outside the guard (`:172`). `MmapRegion` (`kernel/src/process.rs:604-609`) carries `addr`, `size` and `_pages` and no busy or in-flight field, and nothing in `with_process_data` asks whether a sibling holds a window into the range. `Unmapped`'s drop (`kernel/src/mm/unmapped.rs:17-22`) runs `tlb::shootdown()` — which invalidates page-table and TLB entries, not the direct-map alias the kernel already resolved — and then `ManuallyDrop::drop`s the `MmapRegion`, whose `PageAlloc(Vec<PhysPage>)` (`process.rs:99-133`) drops each `PhysPage` into `free_page` (`kernel/src/mm/pmm.rs:122-130`).

**And what reissues it.** `free_page` (`pmm.rs:288-295`) returns the frame to the one global bitmap with no delay, and sets `bm.next_hint = bm.next_hint.min(idx)` (`:294`) — so the just-freed frame becomes the *first* candidate for the next `alloc_page` scan (`:223-229`). `alloc_page(Category::KernelHeap)` is exactly what the global allocator's page source calls (`kernel/src/mm/alloc.rs:13-27`), and `alloc_contiguous` (`pmm.rs:246-285`) can hand the same frame to any of the eleven categories (`pmm.rs:17-29`) — another process's `Mmap`, `SharedMemory`, `Dma`, `Stack`, `Pipe`.

**Threads share all of it.** `spawn_thread` gives the new task the spawning thread's own `PageTables` Arc and `ProcessData` Arc (`kernel/src/process.rs:829-831`, enqueued at `:892-899`); `current_address_space` (`:756`) and `with_process_data` (`:806`) are per-process.

**Impact.** A parked thread's post-wake copy lands in a frame the PMM has already reissued. In the read direction that is an attacker-content, attacker-timed write into the kernel heap or another process's memory; in the write direction it is a read *out* of that frame into a pipe the attacker drains — kernel-heap disclosure to userland. Either way this is the kernel writing through a pointer whose backing it no longer owns: an isolation break and kernel memory corruption reached from an unprivileged process holding nothing it was not given.

**Precondition, and why it is not a race.** All of `SYS_THREAD_SPAWN` (`dispatch.rs:263`), `SYS_MMAP` (`:346`), `SYS_MUNMAP` (`:347`) and `SYS_PIPE` (`:176`) are ungated — no handle, no right, no capability. The whole reproduction fits in one process:

1. `SYS_THREAD_SPAWN` a sibling B.
2. `SYS_MMAP(0, 2 MiB, READ|WRITE, 0)` → V. `prot != NONE` allocates eagerly (`vm.rs:54-63`, `Category::Mmap`), giving frame F.
3. `SYS_PIPE` — both ends land in the same handle table (`kernel/src/arch/syscall/ipc.rs:25-40`).
4. A: `SYS_READ(read_end, V, 2 MiB)`. Empty pipe with `writers > 0` returns `None` (`kernel/src/pipe.rs:224-228`) and A parks at `Deadline::never()` (`io.rs:161-162`).
5. B: `SYS_MUNMAP(V, ..)` — F goes back to the bitmap.
6. B: any allocating syscall, so the heap or another allocation takes F.
7. B: `SYS_WRITE(write_end, bytes, n)` wakes A; `ops::try_read` (`kernel/src/object/ops.rs:339`) → `pipe::try_read` → `buf.write_at` (`pipe.rs:220`) → `copy_nonoverlapping` into F (`user_ptr.rs:214-216`).

The victim waits on `Deadline::never()`, so the attacker chooses how long the window stays open and when to close it. This is a sequence, not a race.

**Why it is confined to the parks.** A syscall runs with IF clear for its whole length (`issues/kernel/syscall-preemption-is-incidental.md`), so an arbitrary preemption cannot open this. The exposure is exactly the sites that yield the CPU voluntarily with a window on the kernel stack: `io.rs`'s four read arms and its write arm are the directly reachable set, and every other handler that takes a `user_bytes`/`user_bytes_mut` window in `dispatch.rs` and then parks belongs to the same class and should be enumerated as part of the fix.

**Fix direction.** Three shapes, in order of how much they cost:

- **Make the frame outlive the window.** The window carries an owning reference to what backs it, so `munmap`'s `PageAlloc` cannot drop while one is live. That is `mm::Dma<'pool>`'s shape, and it is the same lifetime debt `issues/design-debt/kernelslice-outlives-its-allocation.md` records for `KernelSlice` — worth solving once for both, since `UserBytes` and `KernelSlice` are the two raw windows in the tree.
- **Refuse the unmap.** `sys_munmap` (and `sys_mmap`'s FIXED replacement arm, `vm.rs:65-97`, which frees a region the same way) consults a per-process count of live windows into the range and answers `WouldBlock`/`InvalidArgument` rather than freeing under a sibling. Cheapest, and it makes the refusal userland's problem, which is where doctrine puts input that crossed the boundary.
- **Re-derive the window after every park.** `sys_read`/`sys_write` keep the `UserAddr`/len rather than the `kptr` and re-`window()` each time round the loop, so an unmapped range answers `BadAddress` instead of copying. Smallest diff, but it re-opens the same hole at the next handler that caches a translation across a yield, and it does not help a park that happens *inside* the copy.

Whichever is taken, the negative control is the reproduction above: F's contents must be unchanged after the wake (or the syscall must refuse), and the mutation that reverts the fix must show the corruption. The independent oracle is the PMM's own accounting — `Category` totals plus a poisoned free frame — rather than a second reading of the same code.
