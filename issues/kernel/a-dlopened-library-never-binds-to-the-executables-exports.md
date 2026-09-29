---
status: open
kind: defect
opened: 2026-09-29
---

# A dlopen'ed library never binds to the executable's exports

`resolve_dlopen_relocs` (`kernel/src/elf/reloc.rs`) resolves a `dlopen`ed
module's `GLOB_DAT`/`JUMP_SLOT` slots against the other loaded libraries only.
Its startup sibling, `resolve_lib_bind_relocs`, tries the executable's
dynamic exports first (`kernel/src/loader/mod.rs` builds `exe_sym_map` for
it). glibc's `dlopen` searches the global scope, which starts with the
executable, so on ToyOS a plugin that calls back into a symbol its host
exports is left with an unresolved slot and faults when it uses it.
