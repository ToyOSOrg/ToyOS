---
status: open
kind: defect
opened: 2026-10-07
---

# std names its ToyOS backend by a path out of the fork

Fourteen arms under `rust/library/std/src` select ToyOS's file with a
`#[path]` that climbs out of the fork into `sdk/std/`, and two of those files,
`sdk/std/os/ffi.rs` and `sdk/std/sys/pal/mod.rs`, name one back in it
(`os/unix/ffi/os_str.rs`, `sys/pal/unsupported/common.rs`). The owner chose
the mechanism when he ruled the backend out of the fork; what it leaves is
this:

- the fork knows where this repository keeps the backend, and the backend
  where the fork keeps two of its files, so moving `sdk/std`, `rust/` or
  either of those two breaks every fork commit pinned before the move, as
  `issues/std-names-the-sdk-crates-by-path.md` says of the two crates;
- the fourteen lines are not upstream-mergeable as written
  (`.claude/agents/implementer.md`, "A fork");
- a panic raised in the backend names its file through the arm that selected
  it: `library/std/src/sys/time/../../../../../../sdk/std/sys/time.rs`, read
  out of the `libstd` the move built.

**Exit:** no `#[path]` under `rust/library` names a file outside the fork, and
none under `sdk/std` names one inside it.
