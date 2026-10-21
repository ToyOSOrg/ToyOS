---
status: open
kind: defect
opened: 2026-09-27
---

# The compositor ignores a message type it does not know

`Session::dispatch` (`system/compositor/src/session.rs`) ends its match with
`_ => {}`: a frame of a type no protocol here defines is read, framed, and
dropped without a word, on a window's connection and on a fresh one alike. The
one retired type is refused by name;
every other unknown type is accepted and silently discarded.

Owner: `Session::dispatch` in `system/compositor/src/session.rs`.

**Exit**: an unknown type drops its client with `DropReason::OutOfProtocol`.
