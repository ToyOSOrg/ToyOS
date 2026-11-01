---
status: open
kind: defect
opened: 2026-08-10
---

# A process cannot ask what most handles it received are, and using one wrongly is fatal

`SYS_MESSAGE_RECV` installs a message's handles in the receiver's table and
answers only *how many*. `SYS_FSTAT` answers what any handle is, needing no right, and
settles two kinds: only a region of memory the kernel allocated answers
`SharedMemory`, which is how `SharedMemory::lent` refuses every other handle
before it maps one (`sdk/toyos/src/shm.rs`), and only a `Connection` answers
`Socket`. It settles no other. A `Process`, a `Namespace`, a `Connector`, an
`Inbox` and a `SysCap` all answer `Unknown`, and an `Acceptor` answers `Pipe`,
as a pipe end does (`fstat` in `kernel/src/object/ops.rs`).

Every typed use of a handle resolves through `HandleTable::get::<T>`, and
`HandleError::WrongType` ends the caller with a handle fault everywhere but
`HandleTable::get_sent` (`kernel/src/object/handle.rs`). That is right for a
handle the process made or was endowed, and wrong for one a peer sent: a
correct program cannot know.

So: **any process that receives one of those kinds over a connection and then
uses it typed can be ended by whoever sent it.** The sender needs nothing but
`Rights::TRANSFER`, which every handle it made carries.

## Where it is reachable today

- `toyos::launch` answers a started launch with the `Process` handle the
  supervisor sent (`sdk/toyos/src/launch.rs`), and its caller uses it typed.

A generic use is not reached: a read or a write a handle cannot do answers
`PermissionDenied`, and a watch `NotSupported`, never `WrongType`, which is how
netstack's piped ends are refused (`system/netstack/src/pipes.rs`). The
supervisor hands what a launch's client sent to `SYS_SPAWN` and
`SYS_NAMESPACE_BUILD`, which answer a handle of the wrong kind
`InvalidArgument`.

Nothing in the tree is hostile today, so nothing fails. The property the
architecture claims — that a process cannot be harmed by what it was not given —
does not hold across a transfer.

**Exit**: a process that receives a handle of one of those kinds refuses one of
another kind by name before it uses it typed, and a guest test that sends one
in its place sees it refused while the receiver lives.

## Three ways out, and none is free

1. **Report the kind.** `SYS_MESSAGE_RECV` writes `(RawHandle, kind)` pairs
   instead of bare handles. One ABI shape change, in-tree only: no fork calls
   `message_recv` or the SDK's receive. The receiver then refuses by name and
   the fail-fast policy is untouched.
2. **A syscall** answering the kind of a handle. Cheaper to write and
   worse: it is a second place to ask, and it makes "what is this" a round trip
   rather than part of the answer that produced it.
3. **`WrongType` stops being fatal.** Rejected: it is fatal for a reason, and
   three quarters of its call sites really are a bug in the caller.

(1) is the recommendation. It is not this review's to take — it is an ABI
change, and a new syscall is the owner's to approve.

## Ruled not a merge blocker, 2026-08-10

Judged while clearing PR #22's blockers, with the reasoning written down because
the next reader will ask why a class this wide was left open.

- **The fix is an ABI shape change and the ABI was the owner's to approve.** It
  widens the receive's answer from `n` to `n` pairs (`SYS_HANDLE_RECV` then,
  `SYS_MESSAGE_RECV` since), which is a syscall the owner approved changing
  shape after the fact.

So it stays open, and what it is waiting on is a decision rather than an
instrument.

`launcher_refusals` is deleted; `issues/parallel-tests-red-under-other-suites.md` records the commit that restores it.
