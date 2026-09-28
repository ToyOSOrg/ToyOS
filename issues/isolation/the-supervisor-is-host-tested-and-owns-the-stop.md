---
status: assigned
kind: track
opened: 2026-09-28
---

# The supervisor is host-tested and owns the machine's stop

Held by the orchestrator. Stage 2 is blocked by PR #536 (`wt/toyos-fsd`);
#536 changes `userland/init/src/main.rs` by 835 added and 249 deleted lines, so
stage 1 cut before it lands is a merge against it.

## What is true

- `/system/bin/init` (`userland/init/src/main.rs`, 1,746 lines, one file, no
  `#[test]` and no `tests/`) is the root of authority. The kernel starts it
  holding the one full-rights `SysCap` (`spawn_init`,
  `kernel/src/loader/mod.rs`). It builds every program's namespace, device
  claims and narrowed `SysCap` from `system.toml`'s rendered manifest (`start`,
  `build_namespace`, `swap_namespace`, `slot_grant`), starts `[boot] start`,
  swaps a service and restores the binary a failed swap replaced
  (`accept_swap`, `cut_over`, `end_probation`, `restore`), and answers
  `launcher` (`serve_launch`, `resolve`, `declared`).
- On `main` a service that ends is not started again: its kept acceptors close
  (`close_when_it_ends`). #536 adds `restart = true` rows, started again until
  `toyos_manifest::RESTARTS` ends inside `RESTART_WINDOW_SECS`.
- Nothing in the kernel watches init's end: `kernel/src/main.rs` logs its pid
  and keeps nothing. Once init ends, `launcher`, `swap` and `power` have no
  server and no service is swapped or restarted. The machine can no longer be
  stopped from userland, because only init's `SysCap` carries `Rights::POWER`.
- The stop on `main`: `/system/bin/shutdown` or `reboot` (toybox, the one row
  that receives `power`) asks init. init has `logd` flush, bounded by
  `FLUSH_BOUND`, then calls `SYS_SHUTDOWN` or `SYS_REBOOT` (`Init::stop`). The
  kernel's `quiesce` (`kernel/src/syscall/machine.rs`) freezes every userland
  thread at its next return to Ring 3 (`kernel/src/quiesce.rs`), then drains
  writeback, runs `sync_all` over the volumes it holds, flushes USB disk caches
  and cuts power. No program but `logd` hears of the stop.
- On #536 the kernel's `quiesce` syncs nothing: `drain_all` and `sync_all` are
  gone. Its `Init::stop` flushes `logd` and then runs `sync_files`: one
  `Dir::sync` per writable file-server role (every role but `boot`), bounded
  together by `toyos_quiesce::SYNC_MS`. Only then does it call the kernel, and
  a role that has not answered by then is stopped unsynced. Every other service
  still gets no notice. #536 deletes `quiesce_leaves_the_volume_whole` and
  registers no test for the stop's sync.

## Stages

1. **Decisions in a pure crate.** Init's decisions move into a host-tested
   crate, as `toyos-proclife` and `toyos-desktop` did, and the binary keeps
   only handles, spawns and the loop. The decisions are:
   - namespace and claim selection from a manifest row;
   - the claims a restart is owed;
   - `SysCap` narrowing;
   - launch resolution and every launcher refusal (`resolve`, `declared`,
     handle count, relative `cwd`, `MAX_PENDING_LAUNCHES`,
     `HANDSHAKE_TIMEOUT`);
   - the swap ladder and probation;
   - restart policy.

   The crate carries the decided name, `toyos-supervisor`.
   **Exit**: the crate's tests pass in
   `cargo test --workspace --exclude toyos-build`. Each refusal has a mutation
   that reds it, named in the PR. `git grep -nE 'fn (resolve|declared)\b'
   userland/init` answers nothing.
2. **The supervisor owns the stop.** The supervisor asks each service it
   started to finish, in reverse dependency order, storage last, each ask
   bounded. Only then does it call `SYS_SHUTDOWN` or `SYS_REBOOT`, and the
   kernel's part is to stop whatever is left and cut power. The order is not
   in the manifest today:
   - `[boot] start`'s own comment says its order "means nothing";
   - `compositor` and `filepicker` each receive the other;
   - `logd` writes `/log` through the `log` role, whose server's lines reach
     `logd`, and only #536's flush-then-sync breaks that cycle.

   So the order is declared in `system.toml`, or derived from the edges with
   the cycles broken by declaration.
   **Exit**: a guest test in which a service holding unwritten state is asked
   to finish, answers, and has its state on disk after the reboot. Its negative
   control is the same service never answering: the stop still lands at the
   bound, and the supervisor's line names the service.
3. **The quiesce coverage comes back.** Two of the six are disabled in
   `src/redlist.rs`:
   - `quiesce_dump_holds_the_stopped`
     (`issues/kernel/quiesce-dump-holds-the-stopped-reds-wide-with-usb-transport-breaks.md`);
   - `quiesce_wakes_on_the_last_exit`
     (`issues/build/quiesce-wakes-on-the-last-exit-lost-its-serial-ready-beside-other-guests.md`).

   Two more run with open findings:
   - `quiesce_stops_the_machine`
     (`issues/kernel/quiesce-stops-the-machine-stayed-up-beside-other-guests.md`);
   - `quiesce_wakes_on_the_last_park`
     (`issues/build/quiesce-wakes-on-the-last-park-lost-its-serial-ready-beside-other-guests.md`).

   `quiesce_leaves_the_volume_whole` goes with #536. These defects are the
   kernel's stop beside a loaded host, not the missing notice, so stage 2 does
   not close them by itself.
   **Exit**: `cargo run -- --known-red` lists no `quiesce_` test, and stage 2's
   test is registered where `quiesce_leaves_the_volume_whole` was.
4. **The rename.** The owner has decided it: each program is named for what it
   does, not with the Unix daemon suffix. It lands as one mechanical PR after
   the large in-flight PRs, #536 first.

   | today | becomes |
   |---|---|
   | `init` | `supervisor` |
   | `netd` | `netstack` |
   | `logd` | `logkeeper` |
   | `soundd` | `mixer` |
   | `blockd` | `disks` |
   | `fsd` | `files` |
   | `sshd` | `sshserver` |
   | `compositor` | unchanged |

   `files` is already taken: `[programs.files]`, `userland/files` (package
   `files`, the file manager) and `/system/bin/files`. That program needs a new
   name first. Issue slugs carrying an old name are renamed with their
   citations.
   **Exit**: `git grep -nwE 'netd|logd|soundd|blockd|fsd|sshd'` and
   `git grep -nE '/system/bin/init|userland/init|programs\.init|INIT_PATH|"init: '`
   both answer nothing outside this file.

## Decided with stage 2: the ask's ABI

No syscall is proposed. The precedents are IPC over a connection init holds:
`logd`'s `FLUSH`/`FLUSHED` (`toyos-logstream`) and #536's `Dir::sync`. The
candidate is one connection per started service, endowed under a label as
`ORIGINS` is, carrying a finish word and its answer. Its protocol lives in a
crate beside `toyos-swap`, not in `toyos/src`. Still open:

- whether a program started through `launcher` is asked or only stopped;
- whether `SYS_SHUTDOWN`/`SYS_REBOOT` change at all.

`issues/isolation/the-power-broker-authority-with-a-human-in-the-loop.md`'s
inhibitors would ride the same connection.
