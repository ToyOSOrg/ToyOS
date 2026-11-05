---
status: open
kind: defect
opened: 2026-09-29
---

# A dlopen'ed library never binds to the executable's exports

libc's loader (`sdk/libc/src/dl.rs`, `Published`) binds a library's
`GLOB_DAT`/`JUMP_SLOT` slots against every published module, the executable
first, as glibc's `dlopen` searches the global scope. Nothing shows it: no
guest case loads a plugin that calls back into a symbol its host exports, and
no ToyOS executable is linked to export one.

**Exit**: a guest case `dlopen`s a library that calls a function its
executable exports, and the call lands.
