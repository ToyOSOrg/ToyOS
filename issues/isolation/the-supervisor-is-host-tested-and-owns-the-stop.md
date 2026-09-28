---
status: assigned
kind: track
opened: 2026-09-28
---

# The supervisor is host-tested and owns the machine's stop

Held by the orchestrator. Every stage waits on PR #536 (`wt/toyos-fsd`).

On #536 the stop is init's `Init::stop`: it has `logd` flush, then `Dir::sync`s
every writable file-server role, bounded together, then calls `SYS_SHUTDOWN` or
`SYS_REBOOT`. No other service hears of the stop.

## Stages

1. **The rename**, one mechanical PR, first after #536. It touches `toyos/src`,
   `toyos-abi/src`, `userland/libc/src` and the `rust/` fork's ToyOS files, so
   it is briefed as an ABI brief, and its `CLAUDE.md` edits are placed in the
   same PR by an agent briefed for them. Issue slugs carrying an old name are
   renamed with every citation.

   | today | becomes |
   |---|---|
   | `init` | `supervisor` |
   | `netd` | `netstack` |
   | `logd` | `logkeeper` |
   | `soundd` | `mixer` |
   | `blockd` | `disks` |
   | `fsd` | open with the owner |
   | `sshd` | `sshserver` |
   | `compositor` | unchanged |

   **Exit**: over every tracked path and every text file's content, in the
   superproject and the fork's ToyOS files, excluding the bodies of `issues/`
   files (recorded evidence), no hit remains outside the exclusions, each
   judged per match and not per line:
   - a case-insensitive substring search for `netd`, `logd`, `soundd`,
     `blockd`, `fsd`, `sshd`, excluding an identifier containing `klogd`,
     `blockdev`, `VirtioSoundDev`, `netdev`, `netdb`, `ENETDOWN` or `fsdir`;
   - a case-insensitive search for `init` with no letter on either side (so
     `spawn_init`, `struct Init`, `INIT_PATH` and "asks init" all hit),
     excluding a function named `init` (`fn init`, `::init`, `init(`),
     `git init`, `init.defaultBranch`, `rustup-init`, `zero-init`, `init-tls`;
     ELF's `init_array`, `DT_INIT_ARRAY*`, `SHT_INIT_ARRAY` and toyos-elf's
     `init_at`, `init_sz`, `init_info`, `init_count`, `init_out`, `n_init`, and
     `INIT`/`init` in `toyos-elf/tests/fuzz.rs`; `assume_init*`,
     `get_or_init`, `atomic_init`, `sem_init`, `pthread_*_init`,
     `PTHREAD_ONCE_INIT`, `init_routine`, and SFTP's `INIT`/`FXP_INIT`; the
     processor's `INIT` signal (`INIT IPI`, `INIT-SIPI`, what `INIT` leaves an
     AP) and `init_bsp`, `init_ap`, `init_cr0`, `init_pcid`, `INIT_AS`,
     `init_tss_descriptor`, `init_timer`, `X2APIC_TIMER_INIT`, `send_init`,
     `AFTER_INIT`, `init_early`, `init_wall`, `init_reset`, `init_power`,
     `init_budget_ms`, `init_one`, `init_device`, `init_entry`,
     `init_dot_entries`, `Tr2init`; and the C ports' `DG_Init`, `Z_Init`,
     `toyos_music_init`, `toyos_init_sound`, `log_zeroed_init`, and
     `tests/testcases/`' `*_init` names. Prose that says "init" for a bring-up
     (i8042's "Init treats the controller") is reworded, not excluded.
2. **Decisions in a pure crate.** The supervisor's decisions live in
   `toyos-supervisor`, with host tests; `userland/supervisor` keeps only
   handles, spawns and the loop. The decisions: namespace and claim selection
   from a manifest row; the claims a restart is owed; `SysCap` narrowing;
   launch resolution and every launcher refusal; the swap ladder and
   probation; restart policy; the stop order derived from the manifest, with a
   cycle broken only by declaration.
   **Exit**: the crate's tests pass in
   `cargo test --workspace --exclude toyos-build`; a host test refuses a
   manifest whose dependencies form an undeclared cycle; and each refusal and
   each decision above has a mutation that reds a host test, named in the PR.
3. **The supervisor owns the stop.** It asks each service it started to
   finish, in reverse dependency order, storage last, each ask bounded; only
   then does it call the kernel, whose part is to stop whatever is left and
   cut power. The order is not in the manifest today: `[boot] start` says its
   order "means nothing", `compositor` and `filepicker` each receive the
   other, and `logd` writes `/log` through the `log` role, whose server's lines
   reach `logd`.
   **Exit**: two guest tests. In one, two non-storage services with a declared
   dependency are asked to finish in reverse dependency order; it reds when
   they are asked in forward order, and when they are asked all at once with
   storage still last. In the other, a service holding unwritten state is
   asked to finish, answers, and has its state on disk after the reboot; its
   negative control is the same service never answering, where the stop still
   lands at the bound and the supervisor's line names the service.
4. **The stop's coverage comes back.** **Exit**: each claim below is asserted
   by a host test, or by a guest test registered at `Tier::Fast` or
   `Tier::Nightly` with no `src/redlist.rs` row, and a mutation named in the
   PR reds it. A deleted or disabled test covers nothing.
   - a thread's transition wakes the stop;
   - no block operation is open at the stop;
   - the thread count;
   - the console drain;
   - storage made durable before power-off.

## Open with the owner

- `fsd`'s new name. The orchestrator's candidate is `fileserver`; `files` stays
  the file manager's.
- Before stage 3: the ask's ABI (no syscall is proposed); whether a program
  started through `launcher` is asked or only stopped; whether
  `SYS_SHUTDOWN`/`SYS_REBOOT` change at all.
