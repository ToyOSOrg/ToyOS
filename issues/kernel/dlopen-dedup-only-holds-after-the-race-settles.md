---
status: open
kind: defect
opened: 2026-08-31
---

# Two threads racing `dlopen` of one path both pass the dedup check

#343 (`56588ab8`) made `sys_dlopen` (`kernel/src/arch/syscall/vm.rs:176`)
return the handle a resolved path already holds: it looks `resolved` up in
`data.elf.lib_paths` (`vm.rs:183-184`) under one `with_process_data` lock,
and only registers the new mapping — pushing `lib_paths`/`loaded_libs`
(`vm.rs:306-313`) — under a separate, later lock acquisition. Nothing holds
the process-data lock across the two.

Two threads of one process calling `sys_dlopen(same path)` concurrently can
both miss the dedup check (`vm.rs:184`'s `position` finds nothing for
either), both load and map the library, and both push a second `lib_paths`
entry. Bounded — no corruption, no unbounded loop, the address space eats
two mappings instead of looping unboundedly the way the pre-dedup code did —
but the dedup promise the fix's own commit message states ("returns the
handle the name already holds") holds only after the race settles, not on
first contention.

Exit: close the window — one lock held from the `lib_paths` lookup through
the registration push, or a second check under the registration lock before
committing the new entry.

Provenance: adversarial review of PR #343.
