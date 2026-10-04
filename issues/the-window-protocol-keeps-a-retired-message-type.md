---
status: open
kind: defect
opened: 2026-10-01
---

# The window protocol keeps a retired message type

A deleted syscall, `SYS_DEBUG` action or inbox op is simply deleted, and its
number is free. The window protocol still carries one retired type and refuses
it by name:

- `userland/toyos-window/src/lib.rs`: `MSG_RETIRED_CLIPBOARD_SET_SHM = 10`;
- `userland/compositor/src/client.rs`: `DropReason::Retired` and its `why`;
- `userland/compositor/src/session.rs`: `Session::dispatch`'s arm for it.

Deleting the type makes a frame of 10 an unknown type, which `Session::dispatch`
reads and drops without a word
(`issues/the-compositor-ignores-a-message-it-does-not-know.md`). That
issue lands first or with this one.

Owner: `Session::dispatch` in `userland/compositor/src/session.rs`.

**Exit**: `git grep -i retired userland/toyos-window userland/compositor` finds
nothing, and a frame of type 10 drops its client with `DropReason::OutOfProtocol`.
