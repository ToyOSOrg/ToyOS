---
status: open
kind: track
opened: 2026-09-29
---

# The T14 reboots through Ubuntu for every test

Every metal arm is a boot of its own: a flash over ssh to Ubuntu, `BootNext`,
and the log partition read back once the loader's report pass has reset the
machine into Ubuntu, three POSTs in all. The owner's direction is that the
tests run one after another in a ToyOS that stays booted: no reboot per test,
and no Ubuntu.

- **A session is one image** (a kernel build, a parameter line and a config)
  booted once and driven over one exec channel to its `sshd`: `test-runner`,
  launched by init under its own row, whose stdin loop is the session's
  (`userland/test-runner/src/main.rs:140-145,229-249`). The host writes
  `run <name>` and waits for `===TEST_END===`; one channel, because `sshd`
  aliases a second one's input
  (`issues/isolation/sshd-holds-one-channel-and-does-not-say-so.md`).
- **The host judges each test** with `Serial::must_be_clean`
  (`tests/common/serial.rs:341`) and the exit code; ToyOS copies no list.
  - The markers and the test's own output come over the channel: `sshd` pipes
    a program's stdio (`userland/sshd/src/main.rs:234-239`), init keeps a
    launch caller's pipes (`userland/init/src/main.rs:1593-1604`), and a job
    inherits the runner's stdout (`userland/test-runner/src/main.rs:276-277`).
    None of it reaches `/log`, so a test that kills the machine leaves the
    host what crossed before it died; the kernel's account is the black box's.
  - The kernel's records, to be built: after each test `run_one` reads the log
    on its own cursor (`toyos::log::LogTail`, on its row's `logread`) and
    writes the test's records over the session before its `===TEST_END===`,
    with how many the ring dropped; a drop reds the test. Every record lands
    in one window, a builtin's too. A runner init started at boot, as QEMU's
    shared block's is, writes none: its stdout is a log ring, and the console
    carries the records already.
- **A test leaves the machine as it found it.** After each test `run_one`
  writes the processes alive (`roster`), each claim and its holder
  (`inventory`), and the listings of `/tmp`, the session home, `/state` and
  `/log`; the host compares each with the session's opening one, `/log` less
  what `bootlog::split_listing` gives `logd` and the loader. A leftover reds
  its test and gives the next one a fresh session. A test whose premise is a
  fresh boot, as `audio_idle_suspend`'s is, says so in its row and runs first
  in a fresh session.
- **The bound is a lease.** `boot-deadline=` arms it; `run_one` asks init to
  renew it for `WEDGE_BOUND_MS`, twice the `JOB_BOUND_MS` it kills a job at,
  and `quit` asks init to retire it. Only init holds the renewal, and no job
  holds the port the runner asks on. A boot no host reaches lapses and resets.
  `hardlockup` is armed only through `boot-deadline=`; arming it on every boot
  is `issues/kernel/whether-every-boot-arms-the-hard-lockup-detector-is-the-owners.md`.
- **Another image costs one reset, and a death costs two.** Another image goes
  by `update < image` and a reboot. A panic, wedge, reset or loader test goes
  by `update --once`; the loader then boots the kept slot and the host fetches
  the record over `sftp`: `loader.log`, after a third POST for the loader's
  report pass, until the loader track's stage 9, and `/log` after it.
- **What no detector catches waits for a hand**: the span before
  `clock::init`, a machine that powers itself off, firmware that does not come
  back, or a fatal event that stood both bounds down. The host declares the
  machine lost when nothing answers within the lease and a POST allowance,
  derived as `return_secs` is; nothing rests on the TCO
  (`issues/hardware/an-armed-tco-has-never-reset-the-t14.md`).

## Stages

1. **One session on today's rig.**
   - `run_one` gains the bound, the renewal, the kernel's records and the
     readings; QEMU's shared block runs it too, lease included.
   - The host flashes one session image, `tests/testcases` with the talking
     boot's netd, sshd and streaming `logd`, runs every member over the
     channel, hands the machine back with `reboot`, and reads the stick as
     today. It takes the base boots: the C corpus, the shared block (without
     `shared-debug`), `testcases` with its `mkdir` and `readdir` boots, and the
     talking and swapping boots. Chunking the shared lists into boots goes, and
     so do the LAN hold jobs.
   - `mkdir_cap` and `readdir_bound` clean up after themselves or red.
   - Waits on
     `issues/kernel/whether-init-may-renew-the-boot-deadline-is-the-owners.md`.
     A member that claims a device also waits on
     `issues/kernel/deferred-release-outlives-its-syscall.md`, whose fix is
     `issues/kernel/every-wait-in-this-kernel-is-a-spin.md`'s, because a claim
     not back by the check reds it.
   - Closes `issues/hardware/a-swap-on-the-t14-lives-inside-a-metal-boots-bound.md`.

   **Exit**: a T14 run folds those boots into one session and judges every
   member per test. Its verdicts equal the per-boot rig's at the same head,
   and a second session in reverse order gives the same verdicts. Each of
   these reds only the test that staged it: a child left alive, a claim left
   held, a file left in `/tmp`, the home, `/state` or `/log`, and a
   `NEVER_CLEAN` line. A job that spins past its bound is killed and red by
   name, and one that calls the renewal is refused. With the renewals
   skipped, the machine resets when its boot's lease runs out. An e1000e QEMU
   guest runs the same session end to end.

2. **Ubuntu leaves the loop**, which is the loader track's stages 6 and 7.
   - First, the measurement its shape rests on: `ssh t14 update < image` timed
     for a session image on today's rig. An image switch costs that write and a
     reset. ToyOS has written this stick at between about 108 KiB/s and
     4.7 MiB/s, 0.2 s to 9.5 s per MiB: 11 s to 8.4 min for the 53 MiB ROOT a
     `shared` image carries.
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
       every reset now reaches the host through a lease;
     - `issues/boot-media/whether-the-t14-takes-images-signed-by-a-bench-key-is-the-owners.md`
       and
       `issues/boot-media/how-a-loader-change-reaches-the-t14-without-ubuntu-is-the-owners.md`.
   - The stick is written once and put first with `update --boot-first`. From
     then on every image goes by `update`, and a machine that cannot boot its
     stick costs a hand and `diag/flash.sh`
     (`issues/build/the-owners-flash-script-runs-diskutil.md`).
   - An image is good only once the host has reached it: the host's first exec
     on it is `update --good`, and a session image names no `[boot] up`, so
     init never marks it good. An image the host cannot reach lapses, spends
     its tries and falls back.
   - Every test row names its place: a session's image, a `--once` image, or
     QEMU. `METAL_ONLY`, `QemuOnly` and the name-keyed dispatch go, and a metal
     row can be redlisted like any other.
   - The loader keeps the pass before it as `loader-previous.log`, by a rename
     (`SetInfo`), so the kept slot's pass does not erase a death's report.
   - Deleted: `src/metal.rs` except its arm gate, `src/icmp.rs`,
     `bootloader/src/bootnext.rs`, `tests/common/metal.rs`' stick readback,
     boot batching and per-boot run, test-runner's job-list mode, `metaltalk`'s
     `converse` and `judge`, and every timing row for firmware, Ubuntu, the
     stick or the router.
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
   that dies falls back on its own, and so does one whose `sshd` does not
   authorize the host's key.

3. **Sessions merge.** A run holds one session per kernel build, parameter
   line and config that some test needs. Actuators that leave the machine as
   it is share an image where their authors say so, as `SELFTESTS` does; a
   config that differs by one program's row joins the base session; and the
   QEMU registrations the T14 can run move into sessions.

   **Exit**: the host's plan names every reset in a run by what forces it: a
   kernel build, a parameter no session carries, or a death.
