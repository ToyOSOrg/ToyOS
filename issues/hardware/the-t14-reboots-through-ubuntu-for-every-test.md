---
status: open
kind: track
opened: 2026-09-29
---

# The T14 reboots through Ubuntu for every test

A T14 boot is a flash through Ubuntu, `BootNext`, and a stick read back after
the loader's report pass resets into Ubuntu. The owner's direction: the tests
run one after another in a ToyOS that stays booted, driven by the host over
ssh, with no reboot between them and no Ubuntu.

- **One exec channel**, to `test-runner`'s stdin loop, since `sshd` aliases a
  second channel's input (`issues/isolation/sshd-holds-one-channel-and-does-not-say-so.md`);
  `tests/ssh-client-host`'s `pipe` relays it once it writes output as it
  arrives (`tests/ssh-client-host/src/main.rs:251-263`).
- **A test's window** is what crosses between its markers: the job's output,
  never `/log` (`userland/sshd/src/main.rs:234-239`,
  `userland/init/src/main.rs:2050-2063`, `userland/test-runner/src/main.rs:276-277`),
  and the kernel's records, which `run_one` reads on its own
  `toyos::log::LogTail` and writes before `===TEST_END===`; a record the ring
  dropped reds the test.
- **A leftover reds its test.** After each test `run_one` writes the processes
  alive and the listings of `/tmp`, `/state` and `/log`, which the host holds to
  the session's first, `/log` less `bootlog::split_listing`'s files. Claims join
  once `issues/kernel/deferred-release-outlives-its-syscall.md` closes, since a
  released claim can read held until then. A test whose premise is a fresh
  boot, as `audio_idle_suspend`'s is, says so and runs first.
- **A stuck test** is killed by the runner's deadline thread made per job
  (`userland/test-runner/src/main.rs:153-194`), and reds by name.

## Stages

1. **Next to build: one session in place of seven or eight boots.** `shared`
   and `ccorpus` with their chunks, `testcases`, `testcases-mkdir` and
   `testcases-readdir` share a config, a parameter line and the shipping
   kernel; from the runner's `spawn:` to `reboot`'s their lists took 42.9 s and
   58.4 s on two T14 runs, inside the 120 s `boot-deadline=` after a lease as
   late as 19.1 s. The session image is `tests/testcases` with
   `tests/lantalkcase`'s netd, sshd and streaming `logd`, `test-runner` started
   by the exec; its label takes its `tests/metal-profile.toml` rows
   (`tests/common/metal.rs:671-679`), and `loader_watchdog_arms`'s control arm
   rides it. The loop writes `run <name>` per member, then `run reboot`, and
   reads the stick. Every judge reads the stick as today but `syscall_cost`,
   whose lines are the job's own, and `log_poll_outlives_a_close`, whose `echo`
   record needs no anchor on the job before it: those read the window.
   `mkdir_cap` and `readdir_bound` remove what they made.

   **Exit**: every registration those boots carried gives the per-boot verdict
   at the same head, each window's judgement agrees, and a second session in
   reverse order gives the same verdicts. Staged in one member, each of these
   reds it alone and the next runs: a fault, by the kernel's record in its
   window; a child left alive; a file left behind; a `NEVER_CLEAN` line; a job
   spinning past its bound.

2. **Ubuntu leaves the loop** in stages 6 and 7 of
   `issues/boot-media/the-loader-does-only-what-must-precede-the-handover.md`,
   which bring each session image by `update` and each death's record back
   over `sftp`.

3. **Sessions outlive the boot deadline** once stage 2 of
   `issues/hardware/a-frozen-toyos-waits-for-a-hand-on-the-power-button.md`
   ships `watchdogd`. An image names no `boot-deadline=`, `judge_arms`
   (`src/metal.rs:894-921`) takes `watchdogd`'s row as its bound, and `pipe`
   bounds each window, not its whole run (`tests/ssh-client-host/src/main.rs:64`).
   Whether such an image has a hard-lockup detector is
   `issues/kernel/whether-every-boot-arms-the-hard-lockup-detector-is-the-owners.md`.
   A ToyOS that runs but that the host cannot reach keeps its watchdog fed, so
   the run stops and says it waits for a hand once nothing has answered within
   the watchdog's bound and a POST allowance. A run holds one session per
   kernel build, parameter line and config some test needs.

   **Exit**: the host's plan names every reset in a run by what forces it, and
   in QEMU an e1000e guest whose link is cut under a session stops the run and
   says it waits for a hand.
