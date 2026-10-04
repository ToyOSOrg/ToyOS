---
status: open
kind: defect
opened: 2026-09-28
---

# `toyos-abi` decodes only the roster header, not its entries

`SysinfoHeader::decode` (`toyos-abi/src/syscall.rs:85`) is "the one spelling of
the header's offsets, so a reader takes a field rather than an index" — but
that one spelling stops at the header. `SYSINFO_ENTRY_SIZE`
(`toyos-abi/src/syscall.rs:103`) documents the entry's byte layout in prose
only, and every reader of an entry hand-spells its offsets instead of calling
a decoder:

- `tests/toyos-rust-tests/src/bin/kill_ends_every_wait.rs:140-141` —
  `entry[9] == 0`, `entry[8]`.
- `tests/toyos-rust-tests/src/bin/process_lifecycle.rs:258` —
  `buf[pos + 9] != 0, buf[pos + 8]`.
- `tests/toyos-rust-tests/src/bin/abuse_thread_name.rs:62` — `entry[9]`.
- `userland/toybox/src/ps.rs:64-65` — `buf[pos + 8]`, `buf[pos + 9] != 0`.

Four readers, each free to walk off by a field the way the header's own doc
comment warns against. `kernel/src/syscall/machine.rs:309-310` is the one
writer (`entry[8] = state; entry[9] = is_thread;`), so the four are already
one silent renumbering away from reading the wrong column.

**Exit**: a `SysinfoEntry::decode` (or equivalent) in `toyos-abi`, the four
readers above converted to call it, and no roster-entry offset left
hand-spelled outside that one function.

Owner: `toyos-abi/src/syscall.rs`, whoever next touches the roster ABI.
