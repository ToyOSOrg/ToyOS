---
status: open
kind: tooling
opened: 2026-10-04
---

# A boot's readback is lost to one ssh that times out

`tests/metal/src/lib.rs`'s `read_mounted` reads each log file off the T14's stick by
one `ssh … cat` (`Driver::cat`), and `Driver::ssh` runs it once: a dial that
times out after the boot came back fails the boot's readback whole. Nothing
retries it, and nothing keeps the stick's copy anywhere else, so the next
boot's flash (`wipefs --all`, then `dd`) destroys the only record of the boot
that ran. Every row judged on that boot then fails on the harness's word, not
the boot's.

Evidence: the full metal profile at `ce1786ff0` (pull request #705, the
orchestrator's Drive log). The `jobcase` boot's machine answered ssh again
48 s after its reboot, the boot stick enumerated, the log partition mounted
and unmounted, and between the two the read failed with `reading a log file
on the machine exited exit status: 255: Connection timed out during banner
exchange`; the next boot's flash followed. Its four rows
(`usb_reset_hands_devices_back`, `blackbox_done_chain`, `machine_reboot`,
`machine_soft_off_decoded`) failed on that line, 275 of 279 green; the same
image (sha256 checked), booted again, was judged green on all four. The development
machine's network dropped in the same minutes.

- **Owed on metal:** the T14 has not run the loop's `ssh` bounds and keepalive
  (pull request #1255), which change every `ssh` this loop runs. The rows are
  the T14's two flashes of `boot:testcases`, `testcases` and
  `testcases-watchdog`. Each `toyos-metal` log of main must show:
  - `toyos-metal` exiting 0 or 1, never 2, with no `Expired` and no `Untaken`;
  - `run: echo rebooting && sudo -n '/usr/sbin/reboot'`, then either a clean
    answer or `ssh failed itself once the machine had started the reboot
    (…); it is ridden`, then a readback;
  - `dd took X s of its Y s bound` for every piece, the smallest margin
    recorded.

  The judge of `wall_clock_is_firmwares` is owed on both benches: it must not
  be red on its window, which counts `down` from the reboot's ask.

  The AMD bench's two flashes ran through that head on 2026-11-11 and showed
  the rest: exit 0, a clean answer to the reboot, and the largest share of a
  bound any piece took was 17.4 s of 1208 s
  (https://github.com/ToyOSOrg/evidence/tree/84a22513e1debd0779794eddc7b326ea3fcb4160/metal).
  Their readbacks were deleted before a judge read them.

Owner: the orchestrator, which holds the T14. **Exit**: a dial lost while
the stick is being read costs a retry bounded by a timeout that fails loudly,
not the boot, or the stick's log partition is copied off whole before anything
else can flash it; and a mutation that makes the first `cat` of a run's first
boot fail once is read back green.
