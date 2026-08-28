---
status: open
kind: defect
opened: 2026-08-28
---

# An `OP_WATCH` completion echoes the interest mask, so a two-direction watch is told WRITABLE where the kernel itself says there is no writability

# An `OP_WATCH` completion echoes the interest mask, so a two-direction watch is told WRITABLE where the kernel itself says there is no writability

`process_watch` computes the real per-direction readiness and throws it away before answering.

`kernel/src/inbox.rs:550-551` is the honest pair:

```rust
let readable = flags.readable() && ops::has_data(object);
let writable = flags.writable() && ops::has_space(object);
```

and line 554 collapses them into one `ready` bool. Nothing downstream reads either name again. All three sites that post the completion rebuild the result word from the *request*:

- **Immediate completion** — `kernel/src/inbox.rs:566-572`. `flags` is `WatchFlags::from_raw(submission.op_flags)` (line 544), i.e. what the caller asked for.
- **The TOCTOU recheck** — `kernel/src/inbox.rs:605-613`. `Watched::is_ready` (line 197) is `self.iter().any(Source::is_ready)`, an OR across whichever directions were registered, so it says *something* fired and never which; the word posted at 611-613 comes from `pp.flags`, the stored request mask (`PendingWatch::flags`, line 210).
- **The event completion** — `complete_pending_for_source`, `kernel/src/inbox.rs:698-704`. The poll is selected by the source that actually fired (`complete_pending_for_event` at line 682 passes `|pp| pp.watches(&event)`), and then answered with both request bits regardless.

The per-direction answers exist and are unused at all three: `ops::has_data`/`ops::has_space` (`kernel/src/object/ops.rs:575-610`) and `Source::is_ready` (`kernel/src/inbox.rs:765-777`).

## The kernel contradicts its own refusal

`WRITABLE` alone on a pipe's read end is refused: `ops::write_source` is `None` for `KObjectRef::PipeRead` (`kernel/src/object/ops.rs:228`) and `ops::has_space` is false (`kernel/src/object/ops.rs:605`), so `Watched::of(None, None)` is `None` and `kernel/src/inbox.rs:576-579` posts `-NotSupported`. That answer is pinned as policy — "a pipe's read end simply has no writability", `tests/toyos-rust-tests/src/bin/handle_kill_policy.rs:198-207`.

Ask for `READABLE|WRITABLE` on the same handle with a byte in the pipe and the same kernel answers `READABLE|WRITABLE`: readability alone makes `ready` true at line 554, and 566-572 echoes both bits. One bit of one word is a refusal when asked alone and an affirmation when asked in company.

## Impact

`toyos-abi/src/inbox.rs:23-26` names these bits as "the interest going in and the result coming back in `Completion::result`". Coming back, the word carries no readiness: for a `READABLE|WRITABLE` watch it is always `5`, and a caller cannot tell a readable connection from a writable one — the exact question the two bits exist to answer.

Bounded, and the bound is worth stating: an inbox belongs to one process (`owner_pid`, `kernel/src/inbox.rs:249`, set at 399, used at 86) and each pending watch echoes its own ring's request, so a process can only misinform itself. No kernel state reads the word, nothing crosses a process boundary, nothing panics. This is a false answer, not an isolation break.

Inert today, and only today. Every in-tree watcher asks for one direction — compositor, netd, soundd, logd, init, terminal, console, filepicker, window, `toyos/src/surface.rs`, test-runner and the QEMU tests all pass `READABLE` alone; the two `WRITABLE` submitters (`rust/library/std/src/sys/net/connection/toyos.rs:209`, `tests/toyos-rust-tests/src/bin/handle_kill_policy.rs:200`) pass it alone. And no SDK consumer *can* read the word: `Poller::wait` and `drain` take `impl FnMut(u64)` and call `f(completion.token)` (`toyos/src/poller.rs:311, 320, 350`), deliberately — see the comment at `toyos/src/poller.rs:343-349`. The both-bits mask is nevertheless submitted by shipped code already: `poll()` builds it from POLLIN|POLLOUT at `userland/libc/src/posix_io.rs:541-542`, and discards the answer by setting `pfd.revents = pfd.events` (`userland/libc/src/posix_io.rs:553-556`). The first consumer that wants per-direction readiness — an honest `revents`, or a bidirectional connection watcher — reads back a copy of its own question.

## Repro

Unprivileged, one process, no capability beyond `sys_inbox_setup` (`kernel/src/arch/syscall/ipc.rs:406`, ungated) and `sys_inbox_submit` (435):

1. `toyos::pipe_pair()`; a read end carries `Rights::WAIT` by default (`kernel/src/object/ops.rs:32`).
2. Write one byte into the write end, so `pipe::has_data` is true (`kernel/src/pipe.rs:261-265`).
3. Submit `OP_WATCH` on the read end with `op_flags = READABLE | WRITABLE`.
4. The completion's `result` is `5`. `ops::has_space` for that handle is false and it has no write source at all.

Reading it takes the raw ring — `Poller` does not expose `result`. The `Connection` case is the one that will bite: `read_source`/`write_source` are `PipeReadable(rx)`/`PipeWritable(tx)` (`kernel/src/object/ops.rs:205, 227`), both are registered, so an event on `rx` completes at `kernel/src/inbox.rs:698-704` with `WRITABLE` set while `tx` is full (`pipe::has_space` false, `kernel/src/pipe.rs:267-271`).

## Fix direction

Post the readiness that completed. The immediate branch already holds it — use `readable`/`writable` from lines 550-551 rather than `flags`. The two pending sites must answer per direction from `pp.sources` instead of `pp.flags`, and two wrinkles decide the shape rather than being edge cases:

- `Source::Log` is edge-triggered and its `is_ready()` is always false (`kernel/src/inbox.rs:775-776`); a plain recompute would answer a log completion (`kernel/src/log/user.rs:36-42`) with zero bits.
- Between the wake and the post another thread may have drained the pipe, so a plain recompute can answer zero bits for a source that genuinely fired.

So the honest word is the direction that fired — `complete_pending_for_event` already knows it, it is the `event` argument at `kernel/src/inbox.rs:682` — OR-ed with a recheck of the other registered direction. Whichever shape it takes, `Watched` (`kernel/src/inbox.rs:182-204`) is the type that should hold the answer per direction, so no caller can rebuild a mask from the request again.

A test belongs with the fix: nothing in the tree asserts an `OP_WATCH` result word. `tests/toyos-rust-tests/src/bin/handle_kill_policy.rs:211-213` says so and defers to "the kernel's own matrix" — there is no such matrix, and the deferral is what let this stand.
