---
status: open
kind: defect
opened: 2026-10-01
---

# The window protocol keeps a retired message type

The SDK breaks freely until ToyOS is adopted, and a deleted syscall, `SYS_DEBUG`
action or inbox op is simply deleted. The window protocol still carries one
retired type and refuses it by name:

- `userland/toyos-window/src/lib.rs`: `MSG_RETIRED_CLIPBOARD_SET_SHM = 10`;
- `userland/compositor/src/client.rs`: `DropReason::Retired` and its `why`;
- `userland/compositor/src/session.rs`: `Session::dispatch`'s arm for it;
- `tests/toyos-rust-tests/src/bin/compositor_hostile_clipboard.rs`: the case
  that sends it, and `tests/toyos.rs`'s judge of the named drop.

Deleting the type makes a frame of 10 an unknown type, which `Session::dispatch`
reads and drops without a word
(`issues/isolation/the-compositor-ignores-a-message-it-does-not-know.md`). That
issue lands first or with this one.

**Exit**: `git grep -i retired userland/toyos-window userland/compositor` finds
nothing, and the hostile client's type 10 is refused as `DropReason::OutOfProtocol`.
