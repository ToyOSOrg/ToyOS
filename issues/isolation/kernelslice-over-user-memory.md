---
status: open
kind: defect
opened: 2026-08-07
---

# `KernelSlice` is the last `&[u8]` over memory the kernel writes while it is held

`user_ptr` hands out no reference to user memory any more, but that is a
statement about addresses *userland chose*. `mm::region::KernelSlice::as_slice`
is the other direction: a kernel allocation the loader later maps into a
process, so the borrow is created before the aliasing exists.

**The half of this that was somebody else's landed on 2026-08-20, and the
borrow did not.** `vma_map` used to pass `writable = true` unconditionally, so a
`LibMemory::Shared` image one process already had mapped was writable by that
process while `dlopen` relocated the same image for another. W^X closed it:
`mm::paging::Prot` has three variants and none of them is writable-and-
executable, and `LoadedLib::map_into` maps a module's code `ReadExec` in every
process that loads it, so no process can write the cached image at all.

**The type learned to state the rule, and the loader did not learn to keep it.**
`as_slice` is an `unsafe fn` now, and its `# Safety` says the whole of what is
owed: "The returned `&[u8]` must not alias a live `&mut` (through `write`,
`copy_from` or `zero`) for as long as it is held." Two loader sites hold one
across exactly such a write, both re-read on 2026-08-24 (the file's old
`elf.rs:973` predates the split into `kernel/src/elf/{mod,cache,index,reloc}.rs`):

- `resolve_dlopen_relocs` and `resolve_lib_bind_relocs` (`elf/reloc.rs`) take
  `let symbols = lib.symbols();` — a `SymTab` over `.dynsym` and `.dynstr`, both
  `as_slice`d out of the module image — and hold it across `lib.write_at::<u64>`,
  whose `LibMemory::Owned` arm is `self.image.write(...)` into that same
  allocation.
- `load_shared_lib`'s `RELATIVE` pass (`elf/mod.rs`) iterates
  `table_entries(&rela).chain(table_entries(&jmprel))`, which is a `&[u8]` over
  the relocation tables inside the image, and writes through
  `module.slice(entry.offset, 8)?.write::<u64>(0, value)` on every iteration.

**The disjointness that makes those safe is argued in a comment and enforced
nowhere.** `table_entries`' own `SAFETY` says the relocation tables are
read-only data and the writes land in "a different range". What bounds the
writes is `rela::validate`, and it bounds every written entry to
`[rw_offset, rw_offset + rw_size)` — where `rw_offset` is
`rw_lo & !(PAGE_2M - 1)`, rounded *down*. So up to 2 MiB below a module's first
writable byte is inside the range a relocation may legally write into, and
whatever the module chose to put there — its `.dynstr`, its `.dynsym`, its
relocation tables — is memory one of the borrows above may be covering. A `.so`
is untrusted input and chooses that layout.

Nothing here has been shown to miscompile or to corrupt: the claim is that the
argument is a comment rather than a check, on a boundary where the input is
hostile. Either the writes take an exclusive path that cannot overlap a live
borrow, or the tables the loader reads are bounded away from the window it
writes and the comment becomes a refusal.
