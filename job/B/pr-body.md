Closes `issues/a-refused-handle-send-leaves-its-handles-with-callers-that-think-them-moved.md` and `issues/a-refused-handle-move-leaves-the-compositor-holding-it.md`, both deleted here. Files `issues/std-says-a-launch-moves-its-handles-to-the-launcher-even-when-the-move-is-refused.md`.

When the kernel refuses `SYS_HANDLE_SEND`, it puts every handle back at its own number (`HandleTable::transfer` in `kernel/src/object/handle.rs`, called from `sys_handle_send` in `kernel/src/syscall/ipc.rs`). The SDK's four handle-send helpers took borrowed `RawHandle`s and returned that refusal and a refused frame as one error. No caller could tell whether its handles were still its own. Diskserver's handshake, soundserver's `open_stream`, the netstack client, the compositor's three deliveries, fileserver's stream reply, logkeeper's hand-over, the supervisor's log registration and `toyos::launch` all kept whatever a refused move left them. Each refusal leaked a handle slot, and for the compositor's buffers a region too. A client could make that happen again and again, with no limit.

## What changed, per decision

- **The SDK has one handle send, and it consumes what it is given.** `Connection::send_handles(impl IntoIterator<Item = OwnedHandle>) -> Result<(), SyscallError>` (`toyos/src/ipc.rs`):
  - moved handles are forgotten, because the numbers now belong to the peer;
  - refused handles close as they drop;
  - a batch over `MAX_TRANSFER_HANDLES` is refused `InvalidArgument` before the kernel is asked, and every handle in it is closed.

  Why: after the call, ownership is the same on every arm: the caller holds nothing. So no caller can keep a refused handle, and no caller can close a moved one. Closing a moved one would end the caller under the bad-handle policy.
- **The frame is the caller's next send.** Deleted: `send_with_handles`, `send_bytes_with_handles`, `Connection::try_send_with_handles` and the free `ipc::try_send_with_handles`. One primitive replaces the typed/bytes × blocking/non-blocking matrix. Each caller sends its frame with the `send`, `send_bytes` or `try_send` it already used. The order is unchanged: handles go before the frame that announces them. `send_handles`' doc states it.
- **`OwnedHandle` is public, with `unsafe fn from_raw`, as the type the send takes.** Its `into_raw` stays `pub(crate)`: every `into_raw` outside `toyos` is a typed wrapper's own. `SharedMemory::share` and diskserver's `Region::share` now return an `OwnedHandle`, and `From<Pipe> for OwnedHandle` exists. A share or a pipe end arrives owned, so what a caller sends is never a bare number it could also close.
- **Every caller moves onto it, and the hand-written sends lose their code.**
  - `toyos::fs::hello`, the compositor's `copy_begin` and the supervisor's `serve_launch` called `syscall::handle_send` and closed by hand. They now call `send_handles`.
  - The compositor's `deliver_with_handles` becomes `deliver_with_handle`, because every caller passed one handle.
  - The only remaining call of `syscall::handle_send` is inside `send_handles`. Searched: the tree, `rust/library/std` and the fork checkouts under `~/.cargo/git/checkouts`.
- **`toyos::launch` keeps its contract with std's code.** `LaunchError::NotSent` still means nothing was consumed: the request did not encode. `Sent` now also covers a refused move, whose handles `launch` closed. std's `Command` releases its duplicates only on `NotSent`, as before (`rust/library/std/src/sys/process/toyos.rs`), so std's code does not change.
- **`Launch` states what `launch` does with its handles.** Its doc says every handle in `slots`, `extras` and `parent` is the caller's to give and is named once, because `launch` consumes each one: moved, or closed on a refused move. **This is a new outcome**: a launch that names one handle twice, reachable from safe std as `Command::provide("a", h).provide("b", h)`, used to get `LaunchError::Sent` from the kernel's `InvalidArgument` and live. Now `send_handles` closes `h` twice, and the second close ends the caller under the bad-handle policy, as any close of a handle a process does not hold does. No in-tree caller names one twice: the terminal, shell and console each `provide` one fresh `into_raw()`, and std adds a fresh `dup` for every slot and the place.
- **std's prose is now false of the tree, and is filed, not edited.** The comment over `launch`'s answer in `rust/library/std/src/sys/process/toyos.rs` says "The launcher releases what it took", and `CommandExt::provide` (`rust/library/std/src/os/toyos/process.rs`) says the connector leaves the parent "after a successful spawn". On a refused move this process closed the handles, and a `provide`d connector leaves the parent on a failed spawn too. A fork commit rebuilds the toolchain in CI and carries its own rules, so this branch leaves the fork alone. Filed as `issues/std-says-a-launch-moves-its-handles-to-the-launcher-even-when-the-move-is-refused.md`, owned by the next std commit that touches either file, with its exit.
- **`net.rs`'s doc was false.** It said the kernel drops a refused batch, but the kernel restores it. `request_with_handles` now takes owned handles and says it consumes them.
- **The decision has host tests.** `move_batch` takes the syscall as a closure, so three host tests can check the decision using a handle whose close is recorded instead of performed:
  - a refused move closes each handle once, and the batch reached the kernel in order;
  - a taken move closes none;
  - an oversized batch never reaches the kernel, and every handle in it closes.
- **`netstack_gone_mid_bind` gains one assertion in its bind arm.** After the refused bind, it reads the pipe's read end with `read_nonblock` and asserts end of file. The refused send consumed the pipe's only write end. The read does not block, so a write end left open fails the assertion at once with its message (`WouldBlock`) instead of hanging to the harness's ceiling.

The closed issues' exits are met. Every handle send goes through `send_handles`, which consumes owned handles and closes them when the move is refused. The three sites the first issue names (diskserver's `handshake`, soundserver's `open_stream`, `NetstackConn::request_with_handles`) all use it. The first issue's exit also deletes the compositor's issue, whose `deliver_with_handle` now goes through the same send.

Net lines (`git diff --shortstat origin/main...b07c745b`: 24 files, +287 −252, checked against `--numstat`):

| Area | Added | Removed |
|---|---|---|
| Production (`toyos/`, `userland/`) | +148 | −155 |
| Host tests (`toyos/src/ipc.rs`' test module) | +65 | |
| Guest tests | +39 | −27 |
| `issues/` | +35 | −70 |

## Evidence

The host gate is at head b07c745b. The SDK host tests and their controls were measured at 339291ad; the round-2 commit changes no line those tests or controls reach in `toyos/src/ipc.rs`, and the SDK tests ran again at b07c745b (row below).

`<job>` below is `/root/.claude/jobs/015tYBoMwh9xUcBBLG35wTFr/B`.

| Gate | Command | Exit | Log |
|---|---|---|---|
| host, at b07c745b | `cargo run -- --ci host`, run as a non-root user, tree clean (`host.status` empty) | EXIT=0, "Host: 75 step(s), all green" | `<job>/host.log` (`host.exit`, `host.head`) |
| host, at 339291ad (round 1) | same | EXIT=0, "Host: 75 step(s), all green" | `<job>/r1/host.log` |
| SDK host tests, as root, at b07c745b | `cargo test --manifest-path toyos/Cargo.toml --target x86_64-unknown-linux-gnu` | EXIT=0 | `<job>/r2-sdk-test.log` |
| compositor compiles for the host, at b07c745b (not a gate) | `cargo check --manifest-path userland/compositor/Cargo.toml --target x86_64-unknown-linux-gnu` | EXIT=0 | `<job>/r2-compositor-host-check.log` |
| SDK host tests, as root, at 339291ad | `cargo test --manifest-path toyos/Cargo.toml --target x86_64-unknown-linux-gnu` | EXIT=0, 43 passed | `<job>/mutations/baseline.log` |
| the same, after every control was restored | same | EXIT=0 | `<job>/mutations/restored.log` |
| compositor compiles for the host, at 339291ad (not a gate) | `cargo check --manifest-path userland/compositor/Cargo.toml --target x86_64-unknown-linux-gnu` | EXIT=0 | `<job>/compositor-host-check.log` |

The compositor check is there because the host gate does not build `userland/compositor`. The host gate does build and test diskserver, fileserver, logkeeper and soundserver for the host. It does not compile the supervisor or `tests/toyos-rust-tests`, which build only for the ToyOS target.

Not run on this host, which has no ToyOS toolchain, no KVM, and QEMU 8.2.2 instead of the declared 11.1.1: `cargo run -- --build-only`, the guest suite, and every metal row.

### Negative controls (a capability boundary, so high-risk)

The script `<job>/mutations/run.sh` handles each control the same way:
1. checks the patch applies (`git apply --check`);
2. builds the tests (`cargo test --no-run`);
3. runs the SDK's host tests as root;
4. reverts the patch and confirms the tree is clean.

Results are in `<job>/mutations/results.txt`, and the patches are posted as comments.

| Control | What it breaks | Build | Run | What went red |
|---|---|---|---|---|
| `m1-refused-move-keeps-its-handles` | a refused move forgets the batch instead of closing it (the close on refusal reverted) | 0 | 101 | `a_refused_move_closes_every_handle_it_consumed`: closed `[]`, expected `[1, 2, 3]` |
| `m2-taken-move-closes-its-handles` | a taken move drops the batch and closes numbers the peer now holds | 0 | 101 | `a_taken_move_closes_nothing`: "closed [1, 2] after a move" |
| `m3-oversized-batch-sends-its-first-eight` | over the limit, the extra handle is dropped and the first eight are sent | 0 | 101 | `a_batch_past_the_bound_is_refused_and_closed`: "an oversized batch reached the kernel" |
| `m4-oversized-batch-leaks-the-one-past-the-bound` | over the limit, the handle that did not fit is forgotten | 0 | 101 | `a_batch_past_the_bound_is_refused_and_closed`: closed `1..=8`, expected `1..=9` |
| `m5-batch-reaches-the-kernel-reversed` | the batch reaches the kernel in reverse order | 0 | 101 | `a_refused_move_closes_every_handle_it_consumed`: "the kernel was not asked to move the batch in order", got `[3, 2, 1]` |

Each control turned exactly that one test red; the other 42 passed.

**Whole-change revert.** The three host tests do not exist on the base (f260e0b9), so the whole-change control has to be the guest arm. That means running `netstack_gone_mid_bind`, with its new assertion, against the base's SDK. There the refused write end stays in the child's table, so the read never sees end of file. This needs a guest and was not run here.

**Independent oracle.** The SDK's close is only legal because of how the kernel behaves:
- the dispatcher refuses a count over `MAX_TRANSFER_HANDLES` with `InvalidArgument` before it reads anything;
- `HandleTable::transfer` puts every entry back at its own number when the peer's queue refuses.

Nothing on the host runs the real kernel's table. The only check against it is `netstack_gone_mid_bind`'s end-of-file assertion, in CI's guest suite.

## Guest tests the change reaches

All of them. In every boot, the supervisor registers each program's log ring through `send_handles`, and every std directory connection lends its window through `fs::hello`. Beyond that:

- **`netstack_gone_mid_bind`**: its behaviour changes (the new end-of-file assertion). It needs a guest because it asserts on the kernel's handle table after a refused `SYS_HANDLE_SEND`: that the kernel left the end at its own number, so the SDK's close of it is legal and final. The cheaper tiers cannot reach that:
  - a type cannot see the table;
  - a host test has no kernel (`move_batch`'s host tests only check the SDK's decision against a recorded close);
  - nothing in it depends on hardware, so a metal row would not be a cheaper oracle.

  It runs in the shared boot, so `--metal` runs it on the T14 as well.
- **Spelling changes only, behaviour unchanged** (handles still go before the frame, in the same order): `abuse_shared_grant`, `cpal_drop_unreleased`, `mutual_kill`, `shm_release_reclaims`, and `spawn_lands_claimed` (on the `SYS_DEBUG` boot).
- **Metal only:** `process_tree` (also a spelling change) is on `RUST_SKIP`, so only the `process_tree` metal row on `tests/proctreecase` runs it. The launch path is also reached by the `launch_toctou`, `launch_authority` and `fs_share` metal rows.
- **Reached through the servers:**
  - soundserver's stream open: `cpal_drop_unreleased`, `hda_client_stall`, `null_sink_client_exits`;
  - the netstack client's binds: boots of `tests/netcase`, `tests/lanleasecase`, `tests/lantalkcase` and `tests/metalcase`, where sshserver binds at boot;
  - the compositor's deliveries: only boots of `tests/metalcase` and `tests/metaldevicecase` run a compositor (for example `screen_fatal_halt_composited`);
  - fileserver's stream reply and logkeeper's hand-over to a local reader.

  No test causes a refused delivery at any of these sites.

Only CI's `guest / suite` can show:
- that the supervisor and the guest test binaries compile for the ToyOS target;
- that the kernel leaves a refused batch in place (the end-of-file assertion);
- that no server closes a number it gave away after a taken move. Under the bad-handle policy, the kernel would end that server.

## What I am unsure of

- **`move_batch`'s generic and closure.** It is generic over `H: AsHandle` and takes the syscall as a closure, but production uses it only once (`OwnedHandle`, `syscall::handle_send`). That seam is what gives the decision a host tier; without it the decision would only be checked in a guest.
- **`Command::provide`'s connectors on a refused move.** `launch` now closes them; before, they stayed unannounced in the caller's table. std's code already treats `Sent` as "gone"; its prose does not (filed, above).
- **The guest-tier negative control above was not run.**

🤖 Generated with [Claude Code](https://claude.com/claude-code)

https://claude.ai/code/session_015tYBoMwh9xUcBBLG35wTFr
