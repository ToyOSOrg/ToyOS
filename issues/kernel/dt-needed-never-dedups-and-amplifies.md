---
status: open
kind: defect
opened: 2026-08-28
---

# `load_needed_libs` never dedups `DT_NEEDED`, so 16 bytes of ELF buys 2 MiB of physical memory and the 4097th duplicate panics the kernel

---
status: open
kind: defect
---

# `load_needed_libs` never dedups `DT_NEEDED`, so 16 bytes of ELF buys 2 MiB of physical memory and the 4097th duplicate panics the kernel

`read_exe_tables` collects every `DT_NEEDED` value in `PT_DYNAMIC` into
`ExeTables.needed` (`kernel/src/loader/mod.rs:257-263`). That `Vec` is itself
bounded and reserved exactly: `table` (`mod.rs:230-247`) refuses a `PT_DYNAMIC`
above `mm::MAX_HEAP_ALLOC` (`kernel/src/mm/mod.rs:54` = 2,093,056 bytes), so at
`toyos_elf::dynamic::ENTRY_SIZE` = 16 (`toyos-elf/src/dynamic.rs:34`) at most
130,816 entries survive. **What is not bounded is what the loader then does with
them.** Every entry may carry the same string-table offset, and nothing
downstream notices.

`load_needed_libs` (`mod.rs:662-719`) loops `for &name_offset in &exe.needed`
(`:669`) and does a plain `push` into `NeededLibs { libs: Vec<LoadedLib>, paths:
Vec<String> }` (`:655-659`) on both arms — `:679-681` on a cache hit, `:713-716`
on a miss. No `reserve`, no dedup by name or by resolved path, no cap. The
sibling in the same function has exactly the gate this one lacks: `mod.rs:408-416`
reserves `RelocationIndex::with_capacity` from exact counts and answers
`ResourceExhausted` when it will not fit.

**Every duplicate costs at least one 2 MiB contiguous physical frame, off the one
global bitmap.** The cache-hit path (`mod.rs:678` → `kernel/src/elf/cache.rs:201-204`
→ `clone_from_cache` `:208-211`) calls `PageAlloc::new(cached.rw_size,
Category::Elf)` on *every* hit, and `PageAlloc::new`
(`kernel/src/process.rs:103-106`) is `size.div_ceil(PAGE_2M)` frames from
`pmm::alloc_contiguous` (`kernel/src/mm/pmm.rs:246-285`), a single system-wide
free-page bitmap shared by every process. There is no zero-cost entry:
`alloc_contiguous` refuses `count == 0` (`pmm.rs:248`), so an `rw_size` of 0 makes
`clone_from_cache` return `None` and the loop falls through to the miss path,
`elf::load_shared_lib` (`kernel/src/elf/mod.rs:304-327`), which takes
`PageAlloc::new(align_2m(span))` — also ≥ 2 MiB.

That falsifies the module header at `kernel/src/elf/cache.rs:1-2`, which claims
"one image in memory, one private writable window **per process**". With
duplicate names it is one window per `DT_NEEDED` entry. And the windows are not
transient: on success the list moves into the process (`mod.rs:568` destructure,
`:585` `ElfInfo.loaded_libs`) and is held for its lifetime.

## Impact

**A whole-machine memory denial of service, at 131,072x amplification.** 16 bytes
of attacker file take 2 MiB of contiguous physical RAM from the allocator every
other process draws from. `N` ≈ the free-frame count starves every other process
for the duration of one spawn; a spawn that succeeds keeps them.

**A kernel panic on the hardware this kernel targets.**
`size_of::<LoadedLib>()` is 424 bytes (measured with a host `rustc` against a
field-for-field mirror of `kernel/src/elf/mod.rs:83-115`). `Vec` grows by
doubling, so the push that grows capacity 4096 → 8192 asks `GlobalAlloc::alloc`
for 3,473,408 bytes and trips
`assert!(layout.size() <= MAX_HEAP_ALLOC, ...)` at
`kernel/src/mm/alloc.rs:529-531` — the kernel dies, from untrusted ELF content,
at spawn time. That is push #4097, so it needs ≥ 8 GiB of free 2 MiB frames alive
at once: out of reach on the 4 GiB harness (`tests/common/qemu.rs:3505-3506`),
in reach on modern x86-64. Below that threshold the PMM refuses first and the
loader answers cleanly, so the harness as configured cannot see this.

**And symbol binding goes quadratic.** `elf::resolve_lib_bind_relocs`
(`kernel/src/elf/reloc.rs:115-133`) scans `libs.iter()` for every unresolved bind
entry of every lib, and `mod.rs:508` calls it once per lib.

`issues/isolation/no-physical-memory-fairness.md` is not this: that is the absence
of a quota on what a process *asks for*. Here the process asks for nothing — the
kernel allocates on its behalf from a count the file chose. This is the
trust-boundary rule, not the quota rule.

## Precondition

Any unprivileged process. `/tmp` is mounted `UserAccess::ReadWrite`
(`kernel/src/main.rs:427`); `SYS_SPAWN` (`kernel/src/arch/syscall/dispatch.rs:177-202`)
gates handle, endowment and label counts and never the path; `loader::spawn`
opens whatever it is handed through the VFS (`kernel/src/loader/mod.rs:367-375`).

Repro: write any small `.so` to `/tmp/x.so`; write a PIE to `/tmp/a` whose
`PT_DYNAMIC` holds one `DT_STRTAB`/`DT_STRSZ` pair, then N `DT_NEEDED` entries
all carrying the same offset for `x.so`, then `DT_NULL` (`Entries` stops there,
so the terminator goes last); `spawn("/tmp/a")`. N ≈ free-frame count starves the
machine; N = 4097 panics it where the frames exist.

## Fix direction

Collapse repeats and bound the count, in `load_needed_libs`, before the first
`PageAlloc`. One `LoadedLib` per distinct resolved path is what
`elf/cache.rs:1-2` already claims is true, so dedup restores a stated invariant
rather than changing behaviour any correct binary can observe. Above a named cap
on distinct libraries, refuse `ResourceExhausted` — the shape `mod.rs:408-416`
already uses one screen up — and reserve `libs`/`paths` from the count that
decides the refusal, so no growth-by-doubling overshoot is left to absorb.

The two checks this needs: the negative control is the crafted-duplicate spawn
above run against the whole change reverted, which must exhaust the PMM (and, on
a large-memory guest, trip `mm/alloc.rs:529`) and must refuse cleanly with the
change in; the independent oracle is glibc's `ld.so`, which resolves each
`DT_NEEDED` `SONAME` once into a link map and reuses it, so a duplicate entry
costs nothing there.

The DTV half of the same crafted file — 64+ TLS-bearing modules, silently
skipped at `kernel/src/loader/tls.rs:92-98` and ending in std's `rtabort!` — is
`issues/isolation/dtv-capacity-is-a-workload-bound.md` and is not this issue.
