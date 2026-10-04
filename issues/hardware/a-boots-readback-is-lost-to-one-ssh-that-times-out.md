---
status: open
kind: tooling
opened: 2026-10-04
---

# A boot's readback is lost to one ssh that times out

`src/metal.rs`'s `read_mounted` reads each log file off the T14's stick by
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

Owner: the orchestrator, which holds the T14. **Exit**: a dial lost while
the stick is being read costs a retry bounded by a timeout that fails loudly,
not the boot, or the stick's log partition is copied off whole before anything
else can flash it; and a mutation that makes the first `cat` of a run's first
boot fail once is read back green.
