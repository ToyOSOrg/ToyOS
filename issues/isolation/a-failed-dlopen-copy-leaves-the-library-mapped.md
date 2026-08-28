---
status: open
kind: defect
opened: 2026-08-28
---

# `SYS_DLOPEN`'s front-door address check is weaker than the `copy_out` it guards, so a `dlopen` that returns `BadAddress` still leaves the library mapped and registered

# `SYS_DLOPEN`'s front-door address check is weaker than the `copy_out` it guards, so a `dlopen` that returns `BadAddress` still leaves the library mapped and registered

`kernel/src/arch/syscall/dispatch.rs:303-311` validates `a3` and says why:
*"Refused here rather than at the write, so a bad address never leaves a library
the caller was never told about."* The invariant is false.

`UserAddr::checked` asks exactly one question — `v < USER_TOP`
(`kernel/src/mm/mod.rs:71-73` → `toyos-userbound/src/span.rs:30-32`). The write it
is pre-empting is `ctx.copy_out(out, &init_info)` on a `[u64; 2]`
(`kernel/src/arch/syscall/vm.rs:265-268`, `:281`), which routes through
`object::<T>` (`kernel/src/user_ptr.rs:79-89`) and asks three more:

- 8-byte alignment (`toyos-userbound/src/span.rs:57`);
- no 2 MiB straddle for the 16-byte object (`span.rs:60`);
- a translation that survives demand paging (`kernel/src/user_ptr.rs:62-72` →
  `kernel/src/process.rs:1316-1317`, where `find_region` returns `None` for an
  address in no VMA).

So three `a3` values pass the door and fail the write: any address not a multiple
of 8; an 8-aligned address 8 bytes below a 2 MiB boundary; any user-half address
in no region, e.g. `0x8`.

## What is committed before the write

By the time `vm.rs:281` runs, `sys_dlopen` has taken the point of no return:

- the image is mapped — `vm.rs:209` `map_into`, which takes a VA region
  (`kernel/src/elf/mod.rs:149-151`) and, on the cache path, a fresh contiguous
  physical `PageAlloc` for the private RW window (`kernel/src/elf/cache.rs:211`);
- a TLS module id is minted and its `tls_modules` entry pushed when the library
  has TLS (`vm.rs:245-260`);
- the module is appended to `lib_paths`/`loaded_libs` (`vm.rs:270-276`).

`vm.rs:281-283` then returns `SyscallError::BadAddress` and unwinds none of it.

## Impact

The module is not invisible — `sys_dlsym` bounds-checks the raw index against
`loaded_libs.len()` (`vm.rs:369-375`) and `sys_query_modules` (`vm.rs:385`)
enumerates every entry with its path, so the process can still reach it. What is
actually wrong:

- **A refused syscall committed state.** The caller was told the call failed. It
  did not fail; it charged VA and contiguous physical memory and left a module in
  the process's module list.
- **A registered module whose constructors never ran.** The caller never learns
  `init_array` — `toyos-abi/src/syscall.rs:1553` returns `Err` before the
  constructor loop at `:1557-1572` — yet every later `dlopen` resolves symbols
  and TLS against it (`vm.rs:235`, `:237-243`). A later library can bind to a
  module that was never initialised.
- **It survives the `SYS_DLCLOSE` fix.** `dispatch.rs:319` is `SYS_DLCLOSE => 0,`
  and that no-op is tracked in `issues/isolation/dlopen-never-dedups.md`. A
  correct `dlclose` still cannot be called on a module whose index the caller was
  never handed, so whoever closes that issue does not close this one unless they
  come here too.

The leak *rate* is not new — a successful `dlopen` loop leaks identically, which
is the tracked dedup issue. What is new is the false invariant and the
state-committing refusal.

## Precondition

Any process, no capability, no handle: `SYS_DLOPEN` carries no rights check
(`dispatch.rs:301-314`). One raw `syscall(SYS_DLOPEN, path, len, a3, 0)` with a
loadable `path` and `a3` set to a valid stack address plus 1. Repeatable without
bound. The reference wrapper passes an 8-aligned stack `[u64; 2]`
(`toyos-abi/src/syscall.rs:1552`), so ordinary libc-mediated `dlopen` misses the
alignment and unmapped cases — but not the straddle, which is decided by where
the frame lands relative to a 2 MiB boundary.

## Fix direction

Make the failure unreachable rather than tolerable. Either check `a3` at the door
with the same predicate the write uses — `is_user_object` for `[u64; 2]` plus a
translation attempt — so `dispatch.rs:303-304`'s comment becomes true; or move the
`copy_out` ahead of the point of no return and unwind the map, the
`lib_paths`/`loaded_libs` push and the `tls_modules` entry on failure. The first
is smaller and is what the comment already in the tree claims; the second is what
a general "a refused syscall commits nothing" rule wants.

Negative control for either: issue the raw syscall with a misaligned `a3`, then
read `SYS_QUERY_MODULES` back. Today the module count grows by one on a call that
returned `BadAddress`.
