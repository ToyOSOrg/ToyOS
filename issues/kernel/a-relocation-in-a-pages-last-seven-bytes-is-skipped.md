---
status: open
kind: defect
opened: 2026-08-28
---

# `apply_to_page` drops a relocation that starts in a page's last 7 bytes, and no later page ever sees it

The executable's relocations are pre-computed at spawn and applied a page at a
time as the image is demand-faulted. An entry whose write does not fit inside
the 4 KiB chunk it starts in is skipped — and the search that picks each
chunk's share excludes it from every later chunk too, so the write never
happens at all. Nothing counts the loss, nothing logs it, and nothing refuses
the file.

## The chain

`kernel/src/process.rs:1411-1424` is the only caller. It walks one 2 MiB
`DemandPage` frame (`PageAlloc::new(page_2m, ...)`, `:1366`) in 4096-byte steps
and hands each step out as `page.subslice(offset as usize, 4096)` (`:1419`).
`KernelSlice::subslice` sets the new slice's size to exactly the requested
length (`kernel/src/mm/region.rs:39-47`, `size()` at `:31`), so
`page.size() == 4096` at every call.

`kernel/src/elf/index.rs:98-113` then, for the 8-byte entries:

- `let start = self.entries_u64.partition_point(|&(off, _)| off < page_offset);`
  (`:102`) — every entry below this chunk's start is in the excluded prefix;
- `if within_page + 8 <= page.size()` (`:108`) — an entry starting at
  `page_offset + 4089..=4095` fails this and is passed over, `count` unchanged;
- the next chunk's call has `page_offset` 4096 higher, and that same entry now
  sorts *below* it, so `partition_point` drops it from the slice.

`apply_to_page` is stateless: it re-derives `start` from the whole sorted vector
on each call and never marks or consumes an entry. The chunk lattice is
continuous across 2 MiB windows (`region_start` is 2 MiB-aligned, `offset`
steps 4096), so no later chunk and no later fault recovers it. The `i32` loop
(`:115-126`) is the same with width 4 and a 4092 cutoff, and
`has_relocs_in_page` (`:131-139`) hardcodes the same 4096.

The doc comment at `kernel/src/elf/index.rs:97` states the opposite — "the rest
belongs to the next page" — and the rationale it rests on, that the loader
validated the entry against the image, is not true of this path either:
`rela::validate` (`toyos-elf/src/rela.rs:229-251`) is called from exactly one
site, `kernel/src/elf/mod.rs:406`, inside `load_shared_lib` (`:304`). The
executable's entries travel from `parse_rela_entries` into `add_u64`/`add_i32`
(`kernel/src/loader/mod.rs:412-420`, `:461-473`, `:772-781`) with no window,
symbol or alignment check, and `validate` carries no alignment rule in any case.
The bound at `index.rs:108` is the only thing standing between a file-chosen
`r_offset` and the frame — which is why it must stay, and why failing it has to
mean *refuse*, not *skip*.

## Impact

The process starts with un-relocated bytes in one slot of its own image: an
unpatched RELATIVE/GLOB_DAT pointer or TPOFF offset, left at whatever the file
put there. At worst the process faults on it and dies; at best it behaves
subtly wrong. It is confined — the frame is a fresh non-shared allocation
mapped only into that process's address space (`kernel/src/process.rs:1366`,
`map_window_if_absent` at `:1429`) — so no other process and no kernel state is
reachable through it. The defect is that malformed input is silently
half-applied instead of refused: `apply_to_page`'s return is folded into a
saturating `u16` that only decorates a fault trace (`kernel/src/process.rs:1411`,
`:1418-1420`, `:1469`) and is never compared against the `ri.len()` the loader
logged at spawn (`kernel/src/loader/mod.rs:536-537`), so the drop leaves no
trace anywhere.

## What it takes to hit it

One relocation with `(r_offset - vaddr_min) mod 4096 > 4088` (`> 4092` for a
4-byte TPOFF32). Two independent ways in:

- a non-8-aligned `r_offset`. Nothing on the executable path checks alignment.
- a naturally aligned `r_offset` over a non-8-aligned `vaddr_min`.
  `Layout::parse` imposes no alignment on `p_vaddr`
  (`toyos-elf/src/layout.rs:154-270`), `base = USER_VM_BASE - layout.vaddr_min`
  (`kernel/src/loader/mod.rs:404`), and
  `page_elf_offset = (region_start + offset).wrapping_sub(elf_base)`
  (`kernel/src/process.rs:1415`) is therefore congruent to `vaddr_min` mod 4096
  — the whole chunk lattice shifts off natural alignment and an ordinary
  8-aligned GOT slot can straddle.

Both are malformed files today, spawned through `SYS_SPAWN`
(`kernel/src/arch/syscall/dispatch.rs:177-179`), which gates on nothing but the
ambient path. It is also latent for our own toolchain rather than impossible:
`toyos-ld` records a RELATIVE at the input section's own offset verbatim
(`toyos-ld/src/reloc.rs:486`, with `reloc_vaddr = sec_vaddr + reloc.offset` at
`:441`), so an unaligned 8-byte pointer datum would produce an unaligned
RELATIVE that this code can drop. The executable's TPOFF32 relocations are not
a live path — `toyos-ld/src/reloc.rs:463-472` emits them dynamically only under
`is_shared`, and shared objects use the unchunked writer in
`kernel/src/elf/reloc.rs`.

## Fix direction

Two shapes, and either one has to end in a refusal rather than a skip:

- **Refuse at index build.** Give the executable the validation the library path
  already has (`kernel/src/elf/mod.rs:406`) and extend
  `toyos-elf/src/rela.rs::validate` with the rule that
  `[r_offset, r_offset + width)` must not cross a 4 KiB boundary of the fill
  lattice, so one table states it for both callers. A file that breaks it is
  refused by name at spawn, which is what the trust boundary asks for.
- **Remove the artificial chunk.** The frame really is one contiguous 2 MiB
  allocation, and `KernelSlice::write` is `write_unaligned`
  (`kernel/src/mm/region.rs:61-64`), so a straddling write inside the frame is
  already legal. Hand `apply_to_page` the whole frame once with the window's own
  elf offset and widen `has_relocs_in_page` to match; only the frame's far edge
  is then left, and that residue still needs the refusal above.

Whichever is taken, `within_page + width <= page.size()` stays: it is the bound
that keeps a file-chosen offset out of the rest of the frame.

## What a fix has to show

A negative control that reverts the whole change and fails: a spawn of a binary
carrying one relocation at `r_offset` in a chunk's last 7 bytes, asserting the
patched value is present in the process's memory — it must be absent on the
base. For an independent oracle, the x86-64 psABI is the outside judge on what
a relocation table means (every entry is applied, or the object is rejected),
and `toyos-fat32-check`'s posture is the model: a rule read off a specification
rather than off this loader's own assumptions.
