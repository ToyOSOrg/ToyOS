---
status: open
kind: track
opened: 2026-09-29
---

# The T14 reboots through Ubuntu for every test

Every metal arm is a boot of its own. The host flashes the stick over ssh to
Ubuntu, sets `BootNext`, and reads the log partition back after the loader's
report pass has reset the machine into Ubuntu. That is one flash and three
POSTs per boot. Three full runs of 25–27 boots spent 191–218 s in their
kernels, out of 2683–2957 s of boot cycles. The owner's direction is that the
tests run inside ToyOS one after another, with no reboot per test and no
Ubuntu.

- **The transport is the LAN.** The host reaches ToyOS's own `sshd` on the
  I219 at `toyos-t14.local` through `tests/ssh-client-host`. The T14 has no
  serial channel, and nothing in the tree drives USB in device mode.
  `logd`'s stream (TCP 41337) stays open for the whole session. The host cuts
  it per test at the runner's markers.
- **A session is one image**: a kernel build, a parameter line and a config.
  Every test of that image runs in it, one after another, with no reboot. A test
  is one `exec` of `test-runner <name>`, which `sshd` starts through init's
  `launcher`. Each test is therefore a fresh process holding test-runner's
  manifest row: init builds its namespace and claims, and they go when it
  exits. It prints the `===TEST_START/END===` markers QEMU's shared block
  already reads off serial.
- **A test leaves the machine as it found it, and the runner checks that.**
  After each test the runner checks four things:
  - no process the test started is alive (`roster`);
  - every device claim has the holder it had before (`inventory`);
  - `/tmp`, the session home and `/state` list as they did, which is the only
    file fence until per-program views land;
  - the kernel's records in the test's slice pass `must_be_clean`.

  A leftover reds that test by name. The session then resets before the next
  test.
- **The bound is a lease.** `boot-deadline=` is armed from boot, and
  `hardlockup` is armed only through it (`deadline::start`). A boot that stays
  up therefore has neither. The deadline becomes a lease:
  - each test's launch renews it for twice the runner's bound on that test,
    as `WEDGE_BOUND_MS` is twice `JOB_BOUND_MS`, and the host renews it
    between tests;
  - a lapse seals `WEDGED` and resets the machine;
  - an image that reaches good retires the lease its parameter started, so a
    machine the host has let go of idles rather than resets;
  - `hardlockup` is armed on every boot.
- **Another image costs one reset, and a death costs two.**
  - For another image, the host runs `update < image` into the idle slot and
    reboots: one POST, no Ubuntu.
  - A panic, wedge, reset or loader test goes in with `update --once`. Its end
    resets the machine and the loader boots the kept slot. The host then
    fetches the record over `sftp`: from `loader.log` until the loader track's
    stage 9, and from `/log` after it. Until that stage the report pass costs a
    third POST.
  - A session image's `[boot] up` names netd and sshd, so an image the host
    cannot reach is never good and its slot falls back.
- **What no detector catches stops the run and waits for a hand.** That is the
  span before `clock::init`, a machine that powers itself off, firmware that
  does not come back, or a fatal event that stood both bounds down. The host
  declares the machine lost when nothing has answered within the lease and a
  POST allowance, derived as `return_secs` is. It keeps what the stream
  carried. An armed TCO has never reset this machine, and whether it counts is
  undecided (`issues/hardware/an-armed-tco-has-never-reset-the-t14.md`), so
  nothing here rests on it.
- **QEMU needs no resident runner.** Its shared block is already one boot for
  many tests, and each of its machine tests has its own boot as its subject.
  It does take the per-test check. The KVM and CI lanes do not change.

Measured from those three runs' readbacks:

- POST (power-on to loader) took 8.8–21.7 s, median 9.3 s over 78 boots. The
  loader took 1.6–7.8 s, median 2.2 s.
- The I219's link came up 2.7–2.8 s after its driver on every boot.
- 4 of 14 DHCP leases landed 3.2–3.6 s after netd started. The other 10
  landed at 13.3–13.4 s: the first DISCOVER was lost and smoltcp retries after
  10 s. So the lease, and with it `sshd`, arrives at 9.1–9.3 s or at
  19.0–19.1 s of boot.
- Every talking and swapping boot dropped 9–24 frames because no transmit
  descriptor was free (`toyos_i219::TX_RING` is 16). A frame the stream loses
  waits for TCP's retransmit.

## Stages

1. **One session on today's rig.**
   - `test-runner` gains a one-test mode. It renews the lease, runs the job,
     kills it at its bound, runs the check, and exits without rebooting.
   - The deadline becomes the lease and `hardlockup` is armed on every boot
     (owner question 1).
   - The host flashes one session image. It is `tests/testcases` with the
     talking boot's netd, sshd and streaming `logd`. The host runs every member
     over the cable, hands the machine back with `reboot`, and reads the stick
     as today.
   - The session takes the base boots: the C corpus, the shared block (without
     `shared-debug`), `testcases` with its `mkdir` and `readdir` boots, and the
     talking and swapping boots.
   - `mkdir_cap` and `readdir_bound` clean up after themselves or red. A test
     whose premise is a fresh boot says so in its row and runs first.
   - QEMU's shared block judges each member with `must_be_clean` too.
   - The shared lists' chunking into boots goes, and so do the LAN hold jobs.
   - Waits on `issues/kernel/deferred-release-outlives-its-syscall.md`, because
     a claim that is not back before the next launch reds the check. Also waits
     on the transmit drops above.
   - Closes `issues/hardware/a-swap-on-the-t14-lives-inside-a-metal-boots-bound.md`.

   **Exit**: a T14 run folds those boots into one and judges every member per
   test. Its verdicts equal the per-boot rig's at the same head, which is the
   oracle. Each of the following reds only the test that staged it: a child
   left alive, a file left in `/tmp`, and a `NEVER_CLEAN` line. A job that
   spins past its bound is killed and red by name. With the renewal skipped,
   the machine resets at the lease and not at the boot's deadline. An e1000e
   QEMU guest runs the same session end to end.

2. **Ubuntu leaves the loop.** This stage is the loader track's stages 6 and 7.
   - It waits on:
     - that track's stage 5 (tries, the good flag, `update --once` and
       `--boot-first`);
     - `issues/isolation/a-reset-stops-xhci-and-leaves-every-claimed-pci-function-armed.md`
       and `issues/hardware/the-t14-hung-after-rebooting-with-its-i219-faulted.md`,
       because every reset is now taken with netd holding the host's only
       channel;
     - `issues/panic-path/a-fatal-event-stands-down-both-bounds-and-may-leave-nothing-to-end-the-machine.md`;
     - host redials that wait on an event:
       `issues/diagnostics/a-swaps-redial-asks-again-with-no-event-to-wait-on.md`,
       `issues/hardware/the-t14-redial-re-asks-mdns-after-every-refusal.md`,
       `issues/diagnostics/a-first-dial-turned-away-before-a-line-is-waited-on-to-its-callers-bound.md`
       and
       `issues/diagnostics/a-netd-that-dies-while-serving-leaves-the-hosts-stream-silent.md`;
     - owner question 2.
   - The stick is written once and put first with `update --boot-first`. From
     then on every image goes by `update`, and a machine that cannot boot its
     stick is recovered by hand.
   - An image that reaches good retires its boot's lease.
   - Every test row names its place: a session's image, a `--once` image, or
     QEMU with what it needs there. `METAL_ONLY`, `QemuOnly` and the name-keyed
     dispatch go, and a metal row can be redlisted like any other.
   - The loader keeps the pass before it as `loader-previous.log`, by a rename
     (`SetInfo`), so the kept slot's pass does not erase a death's report.
     It also writes the machine's SMBIOS line (Timing, below).
   - A multi-arm stick (the loader booting the next armed kernel instead of
     Ubuntu) is not built. Slots and `--once` do the same with what stays.
   - Deleted, with line counts at `afa84aee7`:
     - `src/metal.rs` (3378 lines) except its arm gate;
     - `src/icmp.rs` (290);
     - `bootloader/src/bootnext.rs` (192);
     - `tests/common/metal.rs`' stick readback, boot batching and per-boot run;
     - test-runner's job-list mode;
     - `metaltalk`'s `converse` and `judge`;
     - every timing row for firmware, Ubuntu, the stick or the router.
   - Closes
     `issues/build/a-hung-boots-log-partition-is-wiped-by-the-next-runs-flash.md`,
     `issues/build/the-sudoers-rendering-has-no-host-side-judge.md`,
     `issues/build/the-metal-loop-writes-the-readback-volume-into-a-directory-it-has-not-made.md`
     and `issues/hardware/the-t14-stopped-answering-ssh-between-two-lan-boots.md`.
   - Ubuntu stays on the NVMe, never booted, until
     `issues/hardware/linuxs-readings-of-the-t14-and-the-tcg-model-are-not-committed.md`
     lets it go. The NVMe install is stage 2 of
     `issues/boot-media/the-machine-updates-itself-without-ubuntu.md`, and a
     session does not care which disk holds its slots.

   **Exit**: a whole T14 run with Ubuntu never started gives the per-boot rig's
   verdicts at the same head. No path in `src/metal*.rs` reaches Ubuntu. A
   panic's reset reaches the loader with no `BootNext` set. A `--once` image
   that panics returns the machine to its session, and its record is judged.
   A slot with a flipped byte, no signature or a lower security version is
   refused and the other slot boots. A slot that dies falls back on its own.

3. **Sessions merge.** A run holds one session per kernel build, parameter
   line and config that some test needs.
   - Actuators that leave the machine as it is share an image where their
     authors say so, as `SELFTESTS` does.
   - A config that differs by one program's row joins the base session.
   - The QEMU registrations the T14 can run move into sessions and cost no
     boot.

   **Exit**: the host's plan names every reset in a run by what forces it: a
   kernel build, a parameter no session carries, or a death.

## Timing

The per-machine record (PR #630) is keyed per test, and its rows time only
ToyOS's own work: the runner's time for each test, and each image's kernel to
`Boot: complete`. Nothing that times firmware, the router or ssh gets a row. The
boot under test names its machine. Before `ExitBootServices`, the loader reads
SMBIOS type 1's vendor and product and type 0's BIOS version from the UEFI
configuration table and writes them as one `loader.log` line. The judge
compares that line byte for byte with `tests/metal/<vendor>-<product>.toml`,
and the Ubuntu query goes in stage 2.

## Owner questions

1. **The lease is an ABI change.** It needs a kernel operation that renews the
   deadline to at most `WEDGE_BOUND_MS` from now and retires it. The operation
   sits behind a capability that only test-runner's row and init hold, and the
   build refuses it elsewhere, as it does `swap` and `slots`. `hardlockup`
   would also be armed without `boot-deadline=`. *Recommended: yes.*
2. **One key signs the T14's images.** `update` never replaces the ESP loader,
   so the T14 installs only what its loader's key signed, and every checkout
   signs with its own throwaway key. *Recommended:* a bench key minted once
   with `--signing-key-new`, kept outside every checkout, and not the owner's
   own key.
3. **A loader change without a flash.** *Recommended:* a later stage in which
   `update` writes a signed loader to a second ESP file. The running loader
   verifies it and chain-loads it once, and only a loader that booted a good
   slot becomes the one firmware boots. Until then a loader change is proven in
   QEMU and reaches the T14 by hand.
