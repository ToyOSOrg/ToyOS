---
status: assigned
kind: track
opened: 2026-09-28
---

# The supervisor is host-tested and owns the machine's stop

Held by the orchestrator. Every stage waits on PR #536 (`wt/toyos-fsd`).

## Stages

Every guest test this track names is registered. A deleted test covers
nothing.

1. **The rename**, one mechanical PR, first after #536. Before stage 1 is
   briefed, the exit's search below runs once over the `rust/` fork's delta as
   well as the superproject, so its hits are known going in. It touches `toyos/src`,
   `toyos-abi/src`, `userland/libc/src` and the `rust/` fork's delta, so it is
   briefed as an ABI brief, and its `CLAUDE.md` edits are placed in the same PR
   by an agent briefed for them. Issue slugs carrying an old name are renamed
   with every citation.

   | today | becomes |
   |---|---|
   | `init` | `supervisor` |
   | `netd` | `netstack` |
   | `logd` | `logkeeper` |
   | `soundd` | `soundserver` |
   | `blockd` | `diskserver` |
   | `fsd` | `fileserver` |
   | `sshd` | `sshserver` |
   | `compositor` | unchanged |

   **Exit**: over every tracked path and every text file's content, in the
   superproject and the `rust/` fork's delta,
   excluding the bodies of `issues/` files (recorded evidence), no hit remains
   outside the exclusions, each judged per match and not per line:
   - a case-insensitive substring search for `netd`, `logd`, `soundd`,
     `blockd`, `fsd`, `sshd`, excluding, case-insensitively, an identifier
     containing `klogd`, `blockdev`, `VirtioSoundDev`, `netdev`, `netdb`,
     `ENETDOWN` or `fsdir`;
   - a case-insensitive search for `init` followed by no lowercase letter
     other than one `s` (so `inits` hits alongside `init`) and preceded by a
     letter only where it starts with a capital `I` (so `spawn_init`,
     `struct Init`, `SpawnInit`, `TimerInit`, `InitPort` and "asks init" all
     hit). A match goes where it names the program and stays otherwise;
     third-party text (`tests/testcases/tinycc/`, the C ports) stays.
2. **Decisions in a pure crate.** The supervisor's decisions live in
   `toyos-supervisor`, with host tests; `userland/supervisor` keeps only
   handles, spawns and the loop. The decisions: namespace and claim selection
   from a manifest row; the claims a restart is owed; `SysCap` narrowing;
   launch resolution and every launcher refusal; the swap ladder and
   probation; restart policy; the stop order derived from the manifest, with a
   cycle broken only by declaration.
   **Exit**: the crate's tests pass in
   `cargo test --workspace --exclude toyos-build`; a host test refuses a
   manifest whose dependencies form an undeclared cycle; each refusal and
   each decision above has a mutation that reds a host test, named in the PR;
   and the stage deletes each decision from `userland/supervisor`, which calls
   the crate for it, named per decision in the PR. No decision exists in two
   places.
3. **The supervisor owns the stop.** It asks each service it started to
   finish, in reverse dependency order, storage last, each ask bounded; only
   then does it call the kernel, whose part is to stop whatever is left and
   cut power.
   **Exit**: two guest tests. In one, two non-storage services with a declared
   dependency are asked to finish in reverse dependency order; it reds when
   they are asked in forward order, and when they are asked all at once with
   storage still last. In the other, a service holding unwritten state is
   asked to finish, answers, and has its state on disk after the reboot; its
   negative control is the same service never answering, where the stop still
   lands at the bound and the supervisor's line names the service.
4. **The stop's coverage comes back.** **Exit**: each claim below is asserted
   by a host test or a guest test, and a mutation named in the PR reds it.
   - A held thread's transition wakes the stop.
   - No block operation is open at the stop.
   - A dump served during the stop counts every thread it stopped as held.
   - Every record reaches the console, `Rebooting.` last.
   - Storage is durable before power is cut.

## Open with the owner

- Before stage 3: the ask's ABI (no syscall is proposed); whether a program
  started through `launcher` is asked or only stopped; whether
  `SYS_SHUTDOWN`/`SYS_REBOOT` change at all.
