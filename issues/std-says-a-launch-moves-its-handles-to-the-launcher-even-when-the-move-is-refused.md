---
status: open
kind: defect
opened: 2026-10-05
---

# std says a launch moves its handles to the launcher, even when the move is refused

`toyos::launch::launch` consumes every handle a `Launch` names: the kernel
moves them to the launcher, or refuses the move and `Connection::send_handles`
closes them in the caller (`toyos/src/ipc.rs`, `toyos/src/launch.rs`). The
prose of std's ToyOS backend about the same handles says something else:

- `sdk/std/sys/process.rs`, the comment over the match on
  `launch`'s answer: "The launcher releases what it took." On a refused move
  the launcher took nothing; this process closed them.
- `sdk/std/os/process.rs`, `CommandExt::provide` (and
  `endow`, whose rule it cites): the handle leaves the parent "after a
  successful spawn". A `provide`d connector also leaves the parent on a spawn
  that failed after the send — a refused move, `Refused`, `Gone`, a lost
  answer — and std's caller cannot keep it. `endow` alone, which goes through
  `SYS_SPAWN` and not the launcher, is unchanged.

std's code is right on every arm: it releases its duplicates only on
`LaunchError::NotSent`, which still means nothing was consumed. Only the
prose is false.

**Owner:** the next commit that touches `sdk/std/sys/process.rs` or
`sdk/std/os/process.rs`.

**Exit:** a commit in which `provide`'s
doc says the connector leaves the parent once the launch is sent, whatever the
spawn answers, and the comment over `launch`'s answer names the close on a
refused move.
