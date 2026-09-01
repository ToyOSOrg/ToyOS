---
status: open
kind: defect
opened: 2026-09-01
---

# A launch that answers `NotDeclared` leaks a handle per stdio slot

`rust/library/std/src/sys/process/toyos.rs`'s `Command::launch` duplicates
every entry of the slot map before it sends the request, because a launch moves
what it carries:

```rust
for &[child_slot, parent] in slot_map {
    match toyos_abi::syscall::dup(toyos_abi::RawHandle(parent)) {
```

Three of the four ways that send can fail to move them close the duplicates:
the `dup` loop's own error arm, and `Err(LaunchError::NotSent(_))`. The fourth
does not —

```rust
Ok(Outcome::NotDeclared) => Ok(None),
```

— and that arm is the one an *undeclared program takes on every spawn*. It
returns `Ok(None)`, so the caller falls through to the direct `SYS_SPAWN`: the
launcher never took the duplicates, and nothing closes them. One leaked handle
per stdio slot per spawn, three for the ordinary case, for the life of the
process.

Found by reading the path while instrumenting an unrelated defect; not measured
against a handle census. It is the same shape as the arm above it, which is why
the comment there — "The send moved them" — is right about `Started` and
`Sent`, and does not hold for `NotDeclared`.

## What closing it takes

Closing `slots` in the `NotDeclared` arm as `NotSent` already does. The
observation is `SYS_SYSINFO`'s live-object census per kind, which
`handle_basic` already drives: spawning an undeclared program N times must not
move the process's handle count by 3N.
