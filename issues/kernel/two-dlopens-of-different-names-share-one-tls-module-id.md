---
status: open
kind: defect
opened: 2026-09-01
---

# Two concurrent `dlopen`s of different TLS-bearing names take the same module id

`sys_dlopen` reads `data.elf.next_tls_module_id` under one `process_data`
acquisition (`kernel/src/arch/syscall/vm.rs:274`) and bumps it under a later one
(`:323`), with the whole load in between. Two threads of one process loading two
**different** TLS-bearing names both read N, both pass the registration's
name check (`:309` — the paths differ, so it is not the same-name case that call
now closes), and both push `TlsModule { module_id: N }`.

The harm is one field down. `dynamic_tls_blocks` is keyed `(Tid, u64)`
(`kernel/src/process.rs:444`), so the two modules share one block per thread and
their thread-locals alias. And `tls_alloc_block`'s lookup is
`tls_modules.iter().find(|m| m.module_id == module_id)`
(`kernel/src/arch/syscall/vm.rs:355`), which takes the first match — so one of
the two modules is sized and templated from the other. Silent userland
corruption, not a refusal.

Not the same defect as the one `#366` closed, and deliberately not folded into
it: that one was two loads of *one* name and its exit was the dedup check under
the registering guard, which this case passes by construction.

Exit: reserve the id where it is read — bump `next_tls_module_id` under the same
acquisition that reads it and let a refused load leak an id, or move the read to
the registering guard and apply `DTPMOD64` relocations there. The comment at
`:269` called the id "reserved" when nothing reserved it; that word is corrected
in the commit that files this.

Provenance: adversarial review of PR #366, which audited those two acquisitions
for the `lib_paths` fix beside them.
