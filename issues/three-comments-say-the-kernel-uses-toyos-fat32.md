---
status: open
kind: defect
opened: 2026-10-12
---

# Three comments say the kernel uses toyos-fat32, and it no longer depends on it

The kernel's dependency on `toyos-fat32` went with its USB storage path
(`13ed7b4f4`): `git grep -n fat32 -- kernel` finds nothing, and the root
`Cargo.lock` lists no `toyos-fat32` among `kernel`'s dependencies. Its users
are `system/fileserver`, `system/update` and `tests/metal`. Three comments
still say the kernel uses it:

- `lib/fat32/Cargo.toml`'s header: "the kernel depends on it by path";
- `build/fatformat.rs`'s module doc: "`toyos-fat32`, which is the kernel's
  driver";
- `populate`'s doc in `build/image.rs`: "the driver the kernel appends
  `kernel.log` with".

**Exit**: none of the three comments names the kernel as the crate's user.

Owner: the orchestrator, which dispatches it.
