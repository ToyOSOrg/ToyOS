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
  which never reaches `/log` (`userland/sshd/src/main.rs:234-239`), and, from a
  runner the exec starts, the kernel's records, read on its own
  `toyos::log::LogTail` and written before `===TEST_END===`; a record the ring
  dropped reds the test. QEMU's serial runner writes no copy, since its output
  reaches the console beside the ring and judges count kernel lines there
  (`tests/common/blockd.rs:579`, `tests/common/iommu.rs:2161`).
- **A leftover reds its test.** After each test the runner lists the processes
  alive and every root `/` has, whole (`kernel/src/vfs.rs:83-86`), `/home` and
  `/apps` among them, and the host holds both to the session's first, `/log`
  less `bootlog::split_listing`'s files. Claims join once
  `issues/kernel/deferred-release-outlives-its-syscall.md` closes; until then a
  member that mints one, as `device_claim_lifetime` does, runs last. A member
  whose premise is a fresh boot, as `audio_idle_suspend`'s is, runs first.
- **A stuck test** is killed by the runner's deadline thread
  (`userland/test-runner/src/main.rs:147-194`), made per job: 14.1 s, twice
  `mutual_kill`'s 7.041 s, the slowest any of these members has taken on the
  T14. It reds by name, and the next member runs.

**Next: two sessions in place of six boots.** `shared`, `shared-2`, `ccorpus`,
`testcases`, `testcases-mkdir` and `testcases-readdir` share a config, a
parameter line and the shipping kernel. The session image is `tests/testcases`
with `tests/lantalkcase`'s netd, sshd and streaming `logd`, under the same
120 s `boot-deadline=`. A member is priced at the allowance behind
`SharedBoot::members`' counts, 860 ms a shipping Rust member and 260 ms a C
corpus case, and the three other lists at twice their slowest on the T14: 23.7 s
for `testcases`' eight, 4.2 s for `mkdir_cap`, 10.1 s for `readdir_bound`. A
session holds 74.8 s: the bound less a tenth, less a lease as late as 19.1 s
(`issues/hardware/most-t14-leases-land-one-dhcp-retry-late.md`), less one
per-job bound. `shared`'s 80 members price at 68.8 s and the rest at 71.8 s, so
a full run goes from 23 boots to 19. The loop writes `run <name>` per member,
then `run reboot`, and reads the stick, where every judge reads as today but
`syscall_cost`, whose lines are the job's own, and `log_poll_outlives_a_close`,
whose `echo` record needs no anchor on the job before it: those read the
window.
`loader_watchdog_arms`' control arm rides it, and `mkdir_cap` and
`readdir_bound` remove what they made.

**Exit**: every registration those boots carried gives the per-boot verdict at
the same head, each window's judgement agrees, and the sessions in reverse
order, fixed members in place, give the same verdicts; in QEMU an e1000e guest
runs a session end to end over `ssh`. Staged in one member, each of these reds
it alone and the next runs: a fault, by the kernel's record in its window; a
child left alive; a file left in `/home`; a `NEVER_CLEAN` line; a job spinning
past its bound.

**Then: Ubuntu leaves the loop** in stages 6 and 7 of
`issues/boot-media/the-loader-does-only-what-must-precede-the-handover.md`,
which bring each session image by `update` and each death's record back over
`sftp`.

**Last: sessions outlive the boot deadline**, once
`issues/hardware/a-frozen-toyos-waits-for-a-hand-on-the-power-button.md` has
armed the hard-lockup detector on every boot and shipped `watchdogd`. An image
names no `boot-deadline=`, `judge_arms` (`src/metal.rs:899-926`) takes
`watchdogd`'s row as its bound, and `pipe` bounds each window rather than its
whole run (`tests/ssh-client-host/src/main.rs:64`). A ToyOS that runs but that
the host cannot reach keeps its watchdog fed, so the run says it waits for a
hand once nothing has answered within the watchdog's bound and a POST
allowance. A run holds one session per kernel build, parameter line and config
some test needs, and a swap leaves a boot's bound
(`issues/hardware/a-swap-on-the-t14-lives-inside-a-metal-boots-bound.md`).

**Exit**: the host's plan names every reset in a run by what forces it, and in
QEMU an e1000e guest whose link is cut under a session stops the run and says
it waits for a hand.
