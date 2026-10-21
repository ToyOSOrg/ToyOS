---
status: open
kind: defect
opened: 2026-09-28
---

# `toyos-abi` decodes only the roster header, not its entries

`SysinfoHeader::decode` (`sdk/abi/src/syscall.rs`) is "the one spelling of
the header's offsets, so a reader takes a field rather than an index" — but
that one spelling stops at the header. `SYSINFO_ENTRY_SIZE` documents the
entry's byte layout in prose only, and every reader of an entry hand-spells
its offsets instead of calling a decoder:

- `system/toybox/src/roster.rs` — `buf[pos + 8]`, `buf[pos + 9] != 0`.
- `tests/guest/rust-tests/src/roster.rs` — `buf[pos + 9] != 0, buf[pos + 8]`.
- `tests/guest/rust-tests/src/bin/kill_ends_every_wait.rs` — `entry[9] == 0`,
  `entry[8]`.
- `tests/guest/rust-tests/src/bin/process_tree.rs` — `entry[9] == 0 &&
  entry[8] != ZOMBIE`.
- `tests/guest/rust-tests/src/bin/abuse_thread_name.rs` — `entry[9]`.

Five readers, each free to walk off by a field the way the header's own doc
comment warns against. `kernel/src/syscall/machine.rs` is the one writer
(`entry[8] = state; entry[9] = is_thread;`), so the five are already one
silent renumbering away from reading the wrong column.

**Exit**: a `SysinfoEntry::decode` (or equivalent) in `toyos-abi`, the readers
above converted to call it, and no roster-entry offset left hand-spelled
outside that one function.

Owner: `sdk/abi/src/syscall.rs`, whoever next touches the roster ABI.
