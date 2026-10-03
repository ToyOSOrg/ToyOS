---
status: open
kind: defect
opened: 2026-10-03
---

# A process's loaded-library ledger is bounded only by its address window

`sys_dlopen` (`kernel/src/syscall/vm.rs`) appends an `elf::LoadedLib` to
`ProcessData::elf.loaded_libs` and a `String` to `elf.lib_paths` for every
distinct resolved path, and nothing bounds how many one process holds. The
dedup is by the normalized path string (`vfs::resolve_absolute`), not by the
file behind it, so a process that makes N symlinks to one real shared object in
`/tmp` and `dlopen`s each resolves N distinct paths and keeps N ledger entries
for one file.

`loaded_libs` is a `Vec` of 544-byte records. Its capacity doubles, so the push
from 2,048 to 2,049 entries grows it to a 4,096-capacity allocation of
2,228,224 bytes, past `mm::MAX_HEAP_ALLOC` (2,093,056), where
`KernelAllocator::alloc` asserts (`kernel/src/mm/alloc.rs`): a kernel panic from
one unprivileged process, the same shape as
`a-processs-mapping-count-is-bounded-only-by-its-address-window.md`.

The per-process region cap added for that defect
(`kernel/src/vma.rs`'s `MAX_REGIONS`, 32,768) does **not** cover this: each
`dlopen` maps its image through `AddressSpace::alloc_region`, so a load is
refused once 32,768 regions are registered — but the `loaded_libs` `Vec`
doubling panics at 2,049 entries, far below that. Each of the 2,048 prior loads
costs real memory (a cached image shares physical pages, but the shared-object
cache's `BUDGET_BYTES` is 256 MiB and past it a load is `Owned`, a private 2 MiB
allocation), so on a guest with a few GiB of RAM the ledger reaches 2,049 before
the PMM runs dry.

By reading, unmeasured here: the 544 bytes are `size_of::<LoadedLib>` and the
threshold is that over the doubling against `MAX_HEAP_ALLOC`. The sibling
defect's test (`tests/toyos-rust-tests/src/bin/abuse_mmap_regions.rs`) is the
pattern a test here would follow, driving `SYS_SYMLINK` + `SYS_DLOPEN` instead
of `mmap`.

Owner: orchestrator. Exit condition: `dlopen` refuses by name the load past a
per-process library bound (as spawn-time `load_needed_libs` already bounds its
distinct `DT_NEEDED` set by `MAX_NEEDED_LIBS`), and a test that `dlopen`s one
shared object through distinct paths until refused reads that refusal and a live
kernel; it reds today on the panic.
