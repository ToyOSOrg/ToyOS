---
status: open
kind: defect
opened: 2026-09-28
---

# `toyos_acpi::Walk::bytes` has no reader outside its own tests

`memory_windows` returns how many bytes of a root bridge's descriptor list it
walked (`toyos-acpi/src/resource.rs`, `Walk::bytes`), so that the loader could
print them. The loader's hex dump of that list is gone
(`bootloader/src/rootbridge.rs`), and nothing else reads the field: only
`toyos-acpi/tests/{fixtures,corpus,resource}.rs` assert on it.

**Exit condition**: the field and the comment that justifies it are deleted,
or a reader outside the crate's tests is named.
