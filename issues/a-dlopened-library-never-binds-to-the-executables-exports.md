---
status: open
kind: defect
opened: 2026-09-29
---

# A dlopen'ed library never binds to the executable's exports

`resolve_dlopen_relocs` (`kernel/src/elf/reloc.rs`) resolves a `dlopen`ed
module's `GLOB_DAT`/`JUMP_SLOT` slots against the other loaded libraries only.
glibc's `dlopen` searches the global scope, which starts with the
executable, so on ToyOS a plugin that calls back into a symbol its host
exports is left with an unresolved slot and faults when it uses it.

**Exit**: `resolve_dlopen_relocs` binds a slot against the executable's dynamic
exports before the other loaded libraries, and a guest case `dlopen`s a library that calls a function its executable
exports, and the call lands.
