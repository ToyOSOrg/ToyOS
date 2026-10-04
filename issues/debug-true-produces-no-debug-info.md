---
status: open
kind: defect
opened: 2026-08-07
---

# `debug = true` produces no debug info, because the linker drops it

`toyos-ld` matches `SectionKind::Debug | DebugString | Linker | Note | Metadata`
and `continue`s (`collect.rs:410-416`), so **no binary this project produces has
a `.debug_*` section**. Verified with `readelf -S` on the kernel, the compositor
and toybox: the sections are `.text .strtab .symtab .rela.dyn .data
.eh_frame_hdr .dynamic .shstrtab` and nothing else.

`[profile.toyos]` states `debug = true` in every crate root, so rustc emits
DWARF into every object file and the linker throws all of it away. The cost is
compile time and has not been measured. The consequence for diagnostics is that
a backtrace can carry a **name** and never a line number or an inlined frame, on
any path — `.symtab`/`.strtab` is the whole of what survives, and it is 32.2% of
the 92,138,384 bytes of ELF this tree ships. Keeping `.debug_line` in `toyos-ld`
is what would change that, and it is not planned.

**2026-08-29: the cost measured, and the cheap exit measured shut.** Cold
guest build (`cargo run -- --build-only`, kernel+bootloader+userland+tests
targets removed first, same host, same session): 220.6 s wall with
`debug = true` against 160.2 s with `debug = false` — the DWARF the linker
throws away costs about 60 s, 27% of every cold guest build. But the flip is
not free: with `debug = false` every one of the 22 guest artifacts hashes
differently, and a kernel-only A/B shows `.data` +0x50, `.rela.dyn` +0x48
(three relocations) and a moved `.text` tail — debuginfo changes rustc's
codegen, not just the metadata. So turning it off ships different bytes on
every binary, and the choice between paying the 27%, shipping the
debuginfo-free codegen, and teaching `toyos-ld` to keep `.debug_line` is one
decision, not a cleanup.
