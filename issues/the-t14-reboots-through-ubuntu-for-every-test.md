---
status: open
kind: track
opened: 2026-09-29
---

# The T14 reboots through Ubuntu for every test

The owner's direction: the tests run one after another in a ToyOS that stays
booted, driven by the host over ssh, with no reboot between them and no Ubuntu.
The T14 is the integration tier: the shared boot runs only as `shared_metal`
and `c_corpus_metal`, and
`issues/the-guest-suite-runs-only-what-no-cheaper-tier-reaches.md` adds
metal rows.

- **A metal row is a member** of the session its kernel build, parameter line
  and config name, and costs no boot, unless it ends the machine (a panic, a
  wedge, a reset) or judges a boot (the loader, an update, a file read back
  after a reboot). Its registration says which.
- **One exec channel**, to `test-runner`'s stdin loop, since `sshd` aliases a
  second channel's input (`issues/sshserver-holds-one-channel-and-does-not-say-so.md`).
  The harness's ssh client relayed it through `pipe`, once that wrote output
  as it arrived; the client was built and is deleted, and
  `issues/the-host-cannot-reach-the-t14-while-it-runs-toyos.md` records the
  commit that restores it.
- **A test's window** is what crosses between its markers: the job's output,
  which never reaches `/log` (`userland/sshd/src/main.rs:234-239`), and the
  kernel's records, which a runner the exec starts reads on its own
  `toyos::log::LogTail` and writes before `===TEST_END===`. A record the ring
  dropped reds the test. QEMU's serial runner writes no copy: its console
  already carries the ring.
- **A leftover reds its test.** After each member the host sends `list`, and
  the runner prints the processes alive and every root `/` has, whole
  (`kernel/src/vfs.rs:83-86`). The host holds both to the session's first,
  `/log` less `bootlog::split_listing`'s files. Claims join once
  `issues/deferred-release-outlives-its-syscall.md` closes; until then a
  member that mints one runs last in its session, as `endowment_denied` does.
- **A stuck test** is killed at the bound the host's `run` names, as
  `--bound-ms=` names the job list's (`userland/test-runner/src/main.rs:78-83`):
  14.1 s, twice `mutual_kill`'s 7.041 s. It reds by name and the next member
  runs. QEMU's harness names no bound and sends no `list`; its own ceiling ends
  the guest.

Every stage below has the host drive a T14 that runs ToyOS, so the track is
blocked on `issues/the-host-cannot-reach-the-t14-while-it-runs-toyos.md`.

**Next: two sessions in place of five boots.** `shared`, `ccorpus`,
`testcases`, `testcases-mkdir` and `testcases-readdir` share a config, a
parameter line and the shipping kernel. The session image is `tests/testcases`
with the netd, sshd and streaming `logd` of `tests/lantalkcase`, which is
deleted (`issues/the-host-cannot-reach-the-t14-while-it-runs-toyos.md`),
under the same 120 s `boot-deadline=`. A session holds 74.8 s of members, priced at
`toyos_tco::RUST_MEMBER_MS` a Rust member, `toyos_tco::C_MEMBER_MS` a C case
and a list at twice its slowest on the T14: the bound less a tenth, less a
lease as late as 19.1 s
(`issues/most-t14-leases-land-one-dhcp-retry-late.md`), less one
per-job bound. Every judge reads the stick as today but `syscall_cost`, which
reads the window. `loader_watchdog_arms`' control arm rides a session,
`mkdir_cap` and `readdir_bound` remove what they made, and `audio_idle_suspend`
waits for soundd's `inspect` to read `suspended`
(`userland/soundd/src/inspect.rs:21-33`), not for a boot no client has reached.

**Exit**, on the T14: every registration those boots carried gives the
per-boot verdict at the same head, each window's judgement agrees, and the
sessions in reverse order, claims last, give the same verdicts. A fault, by the
kernel's record in its window, and a job spinning past its bound, each staged
in one member, red it alone and the next runs. A host test over a recorded
window reds only the member that left a child alive, a file in `/home` or a
`NEVER_CLEAN` line.

**Then: Ubuntu leaves the loop** in stages 6 and 7 of
`issues/the-loader-does-only-what-must-precede-the-handover.md`:
a session image comes by `update`, a row that boots on its own by
`update --once`, and each death's record over `sftp`.

**Last: sessions outlive the boot deadline**, once
`issues/a-frozen-toyos-waits-for-a-hand-on-the-power-button.md` has
armed the hard-lockup detector on every boot and shipped `watchdogd`. An image
names no `boot-deadline=`, `metal::judge_arms` takes `watchdogd`'s row as its
bound, and `pipe` bounds each window rather than its whole run. A ToyOS the host cannot reach keeps
its watchdog fed, so the run says it waits for a hand once nothing has answered
within the watchdog's bound and a POST allowance. A swap then leaves a boot's
bound (`issues/the-host-cannot-reach-the-t14-while-it-runs-toyos.md`).

**Exit**: the host's plan names every reset in a run by what forces it, and a
host test whose transport goes quiet under a session finds the run stopped,
saying it waits for a hand.
