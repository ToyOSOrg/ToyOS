Review of #740, round 1, at head 339291ad (merge base f260e0b9 = origin/main).

Net lines (`git diff --shortstat origin/main...339291ad`): 23 files, +246 −253. Production (`toyos/`, `userland/`) +143 −156, net −13. Host tests +65 (`toyos/src/ipc.rs`, module `tests`). Guest tests +38 −27. `issues/` −70. I checked these against `--numstat`.

Measured at this head: `cargo run -- --ci host` EXIT=0, "Host: 75 step(s), all green" (`host.log`, `host.head` = 339291ad, `host.status` empty). Five SDK controls, each BUILD 0 and RUN 101, each turning exactly one test red with 42 passed. The restored tree runs EXIT=0 with 0 dirty lines (`mutations/results.txt` and the per-control logs). I read every control's failure line in its own log, and each matches the body's table.

Kernel contract the SDK's close depends on, checked in the tree at this head:
- `kernel/src/syscall/dispatch.rs:479` refuses a count above `MAX_TRANSFER_HANDLES` before it reads anything.
- `sys_handle_send` (`kernel/src/syscall/ipc.rs:376`) returns before touching the table on a duplicate, on the connection itself in the batch, and on a missing `TRANSFER` right.
- `HandleTable::transfer` (`kernel/src/object/handle.rs:373`) gives every entry back at its own number when the sink refuses.

So on every refusal that comes back as a value, the batch is still the caller's to close.

Callers searched at this head: `git grep` of the tree, `rust/library/std` at the gitlink commit 7fa3f566, and every `~/.cargo/git/checkouts` commit `Cargo.lock` pins. No `send_with_handles`, `send_bytes_with_handles`, `try_send_with_handles`, `request_with_handles` or `SharedMemory::share` caller is left outside the diff. `syscall::handle_send`'s only caller is `Connection::send_handles`. std reaches `toyos::launch::launch` and the `LaunchError` arms, and both are unchanged in shape.

Owed by CI or the orchestrator, not a BLOCKER at this round (the body marks each one as not run):
- `guest / suite` green at 339291ad. This is the first compile of `userland/supervisor` and `tests/toyos-rust-tests` for the ToyOS target, and the first run of `netstack_gone_mid_bind`'s new end-of-file assertion against the real kernel table. That assertion is the change's only independent oracle.
- `cargo run -- --build-only` at this head.
- The whole-change negative control: `netstack_gone_mid_bind`, with its new assertion, run against the base's SDK, and it must go red. `guest / suite` at this head does not produce it, so someone with a guest has to measure it. Without it, the guest assertion is unproven able to fail. It is owed before land.
- Metal rows, from the orchestrator's T14 only:
  - `process_tree` (on `RUST_SKIP`, so the edited `process_tree.rs` runs nowhere else);
  - `launch_toctou`, `launch_authority` and `fs_share`, which reach `launch`'s changed refusal arm.

BLOCKER

(none)

NOTE

- tests/toyos-rust-tests/src/bin/netstack_gone_mid_bind.rs:212 — the new assertion uses a blocking `notify.read`, so the defect it guards fails as a hang up to the harness's ceiling, not at once with its message — `Pipe::read_nonblock` (`toyos/src/lib.rs:159`) answers `Ok(0)` when no writer is left and refuses at once when one is. Use it. The body already names this under "What I am unsure of".
- toyos/src/lib.rs:90 — `OwnedHandle::into_raw` went from `pub(crate)` to `pub` with no caller outside `toyos`. Every `into_raw()` under `userland/` and `tests/` at this head is a typed wrapper's own. Return it to `pub(crate)`, as the body itself concedes.
- toyos/src/launch.rs:341 — `launch`, a safe public fn, wraps every caller-supplied `RawHandle` in `OwnedHandle(h)` without the contract this diff gives `OwnedHandle::from_raw`. A `Launch` that names one number twice is reachable from safe std, for example `Command::provide("a", h).provide("b", h)`. Before this change the kernel refused it `InvalidArgument` and the caller got `LaunchError::Sent` and lived. Now `move_batch` closes `h` twice, and the second close ends the caller under the bad-handle policy. The body does not name this new outcome. Either state in `Launch`'s docs (`extras`/`slots`/`parent`) that every handle must be distinct and is closed on a refused move, or build each one through `from_raw` with that `SAFETY` argument at the site.
- PR body, "toyos::launch keeps its contract with std": `CommandExt::provide`/`endow` (`rust/library/std/src/os/toyos/process.rs:25`, `:41`) say a moved handle leaves the parent "after a successful spawn". After this change, a refused move closes a `provide`d connector on a failed spawn as well. The body's "std does not change" is true of the code, not of that std doc.

LAND AFTER NAMED CHANGES
