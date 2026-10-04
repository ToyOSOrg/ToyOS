---
status: open
kind: defect
opened: 2026-08-07
---

# `debug = true` produces no debug info, because the linker drops it

Every `[profile.toyos]` but the loader's sets `strip = "debuginfo"`, so **no
binary this project produces has a DWARF section**. Verified with `readelf -SW`
on the x86-64 kernel, compositor and toybox: no `.debug_*` section but rustc's
`.debug_gdb_scripts`, which is no DWARF.

`[profile.toyos]` states `debug = true` in every crate root, so rustc emits
DWARF into every object file and the linker throws all of it away. The cost is
compile time and has not been measured. The consequence for diagnostics is that
a backtrace can carry a **name** and never a line number or an inlined frame, on
any path — `.symtab`/`.strtab` is the whole of what survives, and it is 32.2% of
the 92,138,384 bytes of ELF this tree ships.

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
debuginfo-free codegen, and keeping `.debug_line` is one
decision, not a cleanup.
