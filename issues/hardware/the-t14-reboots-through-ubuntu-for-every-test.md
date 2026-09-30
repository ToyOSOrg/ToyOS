---
status: open
kind: track
opened: 2026-09-29
---

# The T14 reboots through Ubuntu for every test

Every metal arm is a boot of its own: a flash over ssh to Ubuntu, `BootNext`,
and the log partition read back once the loader's report pass has reset the
machine into Ubuntu. The owner's direction is that the tests run one after
another in a ToyOS that stays booted: no reboot per test, and no Ubuntu.

- **A session is one image** (a kernel build, a parameter line and a config)
  booted once and driven over one exec channel to its `sshd`: `test-runner`,
  launched by init under its own row, whose stdin loop is the session's
  (`userland/test-runner/src/main.rs:140-145,229-249`). The host writes
  `run <name>` and waits for `===TEST_END===`, on one channel because `sshd`
  aliases a second one's input
  (`issues/isolation/sshd-holds-one-channel-and-does-not-say-so.md`).
- **The host judges each test** with `Serial::must_be_clean`
  (`tests/common/serial.rs:341`) and the exit code; ToyOS copies no list.
  - The markers and the test's own output come over the channel and never
    reach `/log`: `sshd` pipes a program's stdio
    (`userland/sshd/src/main.rs:234-239`), init keeps a launch caller's pipes
    (`userland/init/src/main.rs:1593-1604`), and a job inherits the runner's
    stdout (`userland/test-runner/src/main.rs:276-277`).
  - The kernel's records, to be built: after each test `run_one` reads the log
    on its own cursor (`toyos::log::LogTail`, on its row's `logread`) and
    writes the test's records before its `===TEST_END===`, with how many the
    ring dropped; a drop reds the test. Every record lands in one window, a
    builtin's too.
  - A death's own account is the black box's where it sealed one; after a
    reset that sealed nothing, the host has the tail of `logd`'s stream
    (`toyos_logstream::PORT`), held open for the session.
- **A test leaves the machine as it found it.** After each test `run_one`
  writes the processes alive (`roster`), each claim and its holder
  (`inventory`), and the listings of `/tmp`, the session home, `/state` and
  `/log`; the host compares each with the session's opening one, `/log` less
  what `bootlog::split_listing` gives `logd` and the loader. A leftover reds
  its test and gives the next one a fresh session. A test whose premise is a
  fresh boot, as `audio_idle_suspend`'s is, says so in its row and runs first
  in a fresh session.
- **A session runs under the production watchdog as it ships**
  (`issues/hardware/a-frozen-toyos-waits-for-a-hand-on-the-power-button.md`),
  and nothing in the kernel is added for tests. A stuck test is killed over
  the session: past its bound the host has the runner end it, and it reds by
  name. A frozen ToyOS is reset by the watchdog. A ToyOS that still runs but
  that the host cannot reach keeps its watchdog fed, so the run stops there
  and waits for a hand, as it does for a machine that powers itself off or
  firmware that does not come back; the host says so once nothing has
  answered within the watchdog's bound and a POST allowance, derived as
  `return_secs` is.

## Stages

1. **One session on today's rig.**
   - First, and next to build: the `testcases` arm's members in one session,
     under the `boot-deadline=` every metal image already carries
     (`tests/common/metal.rs:805-823`). It builds `run_one`'s kernel records;
     a `tests/ssh-client-host` mode that relays one exec channel's input and
     output as they arrive, which the metal loop opens as `test-runner` during
     the boot, writing `run <name>` for each member in the arm's order and
     keeping each window in the readback; and the session image,
     `tests/testcases` with `tests/lantalkcase`'s netd, sshd and streaming
     `logd` rows, in which that exec, not `[boot] start`, starts
     `test-runner`. The loop hands the machine back with `reboot` and reads
     the stick as today. Its exit: every registration that rides `testcases`
     gives, off the stick, the verdict the per-boot arm gives at the same
     head, the host's judgement of each member's window agrees with it, and a
     member patched to fault reds on its window by the kernel's record of the
     fault.
   - A session outlives that bound once stage 2 of
     `issues/hardware/a-frozen-toyos-waits-for-a-hand-on-the-power-button.md`
     ships `watchdogd`. A session image then starts it as the shipped image
     does and names no `boot-deadline=`, whose whole-boot bound would end the
     session, so `judge_arms` (`src/metal.rs:909-936`) takes `watchdogd`'s row
     as its bound, and `hardlockup` runs only if every boot arms it
     (`issues/kernel/whether-every-boot-arms-the-hard-lockup-detector-is-the-owners.md`).
   - QEMU's shared block runs the same `run_one`; its runner's stdout is a log
     ring, so the console carries the kernel's records and it writes none.
   - One session image then folds the base boots: the C corpus, the shared
     block (without `shared-debug`), `testcases` with its `mkdir` and
     `readdir` boots, and the talking and swapping boots; `mkdir_cap` and
     `readdir_bound` clean up after themselves or red.
   - A member that claims a device waits on
     `issues/kernel/deferred-release-outlives-its-syscall.md` (fixed under
     `issues/kernel/every-wait-in-this-kernel-is-a-spin.md`), because a claim
     not back by the check reds it.
   - Closes `issues/hardware/a-swap-on-the-t14-lives-inside-a-metal-boots-bound.md`.

   **Exit**: a T14 run folds those boots into one session and judges every
   member per test with the per-boot rig's verdicts at the same head, and a
   second session in reverse order gives the same verdicts. Each of these reds
   only the test that staged it: a child left alive, a claim left held, a file
   left in `/tmp`, the home, `/state` or `/log`, and a `NEVER_CLEAN` line. A
   job that spins past its bound is killed over the session and red by name,
   and the next member runs. In QEMU, an e1000e guest runs the same session
   end to end, and one whose link is cut under a session stops the run and
   says it waits for a hand.

2. **Ubuntu leaves the loop**, which is the loader track's stages 6 and 7.
   - First, the measurement its shape rests on: `ssh t14 update < image` timed
     for a session image on today's rig, since an image switch costs that
     write and a reset. ToyOS has written this stick at between about
     108 KiB/s and 4.7 MiB/s, 0.2 s to 9.5 s per MiB: 11 s to 8.4 min for the
     53 MiB ROOT a `shared` image carries.
   - Then a loader reaches the T14 as an image does (owner ruling): ToyOS
     receives it over ssh as `update` receives an image and writes it to the
     stick, and the running loader tries it once and keeps the old one as the
     fallback. Ubuntu leaves the loop only once this works.
   - Every T14 image is signed with the bench key the T14 trusts (owner
     ruling), made once outside every checkout at
     `~/.config/toyos/t14-bench-signing-key` by `--signing-key-new` with
     `TOYOS_SIGNING_KEY` naming that path.
   - Waits on the loader track's stage 5 (tries, the good flag, `update --once`
     and `--boot-first`), and on:
     - `issues/isolation/a-reset-stops-xhci-and-leaves-every-claimed-pci-function-armed.md`
       and `issues/hardware/the-t14-hung-after-rebooting-with-its-i219-faulted.md`,
       because every reset is now taken with netd holding the host's only
       channel;
     - `issues/panic-path/a-fatal-event-stands-down-both-bounds-and-may-leave-nothing-to-end-the-machine.md`;
     - `issues/diagnostics/a-swaps-redial-asks-again-with-no-event-to-wait-on.md`,
       `issues/hardware/the-t14-redial-re-asks-mdns-after-every-refusal.md`,
       `issues/diagnostics/a-first-dial-turned-away-before-a-line-is-waited-on-to-its-callers-bound.md`
       and
       `issues/diagnostics/a-netd-that-dies-while-serving-leaves-the-hosts-stream-silent.md`,
       so that every host redial waits on an event;
     - `issues/hardware/most-t14-leases-land-one-dhcp-retry-late.md`, because
       every reset now reaches the host through a lease.
   - The stick is written once and put first with `update --boot-first`; from
     then on every image goes by `update`, and a stick that boots neither
     loader costs a hand and `diag/flash.sh`
     (`issues/build/the-owners-flash-script-runs-diskutil.md`).
   - Another image costs one reset: `update < image` and a reboot. A death
     costs two: a panic, wedge, reset or loader test goes by `update --once`,
     the kept slot boots after it, and the host fetches the record over
     `sftp`, from `loader.log` until the loader track's stage 9 (whose report
     pass costs a third POST) and from `/log` after it.
   - An image is good only once the host has reached it: the host's first exec
     on it is `update --good`, and a session image names no `[boot] up`, so
     init never marks it good.
   - Every test row names its place, a session's image, a `--once` image or
     QEMU, so `METAL_ONLY`, `QemuOnly` and the name-keyed dispatch go.
   - The loader keeps the pass before it as `loader-previous.log`, by a rename
     (`SetInfo`), so the kept slot's pass does not erase a death's report.
   - Deleted: `src/metal.rs` but its arm gate, `src/icmp.rs`,
     `bootloader/src/bootnext.rs`, the stick readback and batching of
     `tests/common/metal.rs`, and test-runner's job-list mode.
   - Closes
     `issues/build/a-hung-boots-log-partition-is-wiped-by-the-next-runs-flash.md`,
     `issues/build/the-sudoers-rendering-has-no-host-side-judge.md`,
     `issues/build/the-metal-loop-writes-the-readback-volume-into-a-directory-it-has-not-made.md`
     and `issues/hardware/the-t14-stopped-answering-ssh-between-two-lan-boots.md`.
   - Ubuntu stays on the NVMe, never booted, until
     `issues/hardware/linuxs-readings-of-the-t14-and-the-tcg-model-are-not-committed.md`
     lets it go.

   **Exit**: a whole T14 run with Ubuntu never started gives the verdicts a
   per-boot run gave at the commit stage 2 branches from. No path in
   `src/metal*.rs` reaches Ubuntu. A panic's reset reaches the loader with no
   `BootNext` set. A `--once` image that panics returns the machine to its
   session, and its record is judged. A slot with a flipped byte, no signature
   or a lower security version is refused and the other slot boots. A slot
   that dies falls back on its own, and one whose `sshd` does not authorize
   the host's key is never marked good. A loader sent over ssh boots once, and
   one that brings no slot to good leaves the old one booting.

3. **Sessions merge.** A run holds one session per kernel build, parameter
   line and config that some test needs: actuators that leave the machine as
   it is share an image where their authors say so, as `SELFTESTS` does, a
   config that differs by one program's row joins the base session, and the
   QEMU registrations the T14 can run move into sessions.

   **Exit**: the host's plan names every reset in a run by what forces it: a
   kernel build, a parameter no session carries, or a death.
