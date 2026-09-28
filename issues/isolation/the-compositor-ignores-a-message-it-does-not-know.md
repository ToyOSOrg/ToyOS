---
status: open
kind: defect
opened: 2026-09-27
---

# The compositor ignores a message type it does not know

`Session::dispatch` (`userland/compositor/src/session.rs`) ends its match with
`_ => {}`: a frame of a type no protocol here defines is read, framed, and
dropped without a word, on a window's connection and on a fresh one alike. The
one retired type, `window::MSG_RETIRED_CLIPBOARD_SET_SHM`, is refused by name;
every other unknown type is accepted and silently discarded.

`compositor_stall`'s streaming case depends on it: its window sends
`UNKNOWN_MSG` on every pass as load with nothing to draw
(`tests/toyos-rust-tests/src/bin/compositor_stall.rs`). A refusal there would
drop the window after its first frame and leave the case passing with no load.

Owner: `Session::dispatch` in `userland/compositor/src/session.rs`.

**Exit**: an unknown type drops its client with `DropReason::OutOfProtocol`,
and the stall's stream is a type the compositor serves without drawing.
