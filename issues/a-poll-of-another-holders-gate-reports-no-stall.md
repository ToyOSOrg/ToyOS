---
status: open
kind: defect
opened: 2026-10-23
---

# A poll of another holder's gate reports no stall

A stream shared between processes copies under a per-direction gate, one
holder's at a time, and a holder whose call waits for another's gate warns
every `GATE_STALL_NANOS` it lasts (`sdk/toyos/src/net/stream.rs`,
`gate_wait`): a holder stopped inside its gate section by a debugger leaves
the others waiting, and the wait says so.
That covers a blocking read or write and libc's interruptible ones
(`stream::wait_ready`). Two waits on the same doorbell do not report it:

- **libc's `poll`** (`sdk/libc/src/posix_io.rs`, `wait_fds`) watches each
  stream direction through `stream::watch` and waits on its doorbell until the
  caller's deadline, `u64::MAX` for `-1`. A direction ready under another
  holder's gate is watched as one not ready, and nothing is said.
- **mio's selector** (`forks/mio`, `src/sys/toyos/selector.rs`, `select`)
  registers a stream's directions through `stream::register`, which sets the
  gate's want, and waits on the bells for the caller's timeout. Tokio's
  runtime waits there without a timeout when idle.

Neither wait is wrong in what it answers: the gate's release rings the
waiter's bell, so readiness is not lost. The defect is the root rule that a
wait exposes stalled progress; a server that polls a shared stream, as an
inheriting server's workers do, waits silently on a stopped holder.

Exit: `stream::watch` already names the gate's owner
(`Watching::gate`); `poll` bounds its wait at the stall interval while any
watched direction waits on a gate and reports it, as `wait_ready` does. mio
needs `register` to answer the same, or a `toyos::net::stream` call that
reports a stall the selector can ask between bounded waits; that is a change
in the fork and in the SDK it calls.

Owner: the networking SDK (`sdk/toyos/src/net/`), with the mio fork.
