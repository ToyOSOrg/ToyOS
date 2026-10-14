---
status: open
kind: defect
opened: 2026-10-14
---

# The T14's first download after boot runs slower than the rest

On the T14, `internet_download`'s first run of a boot is far slower than its
later runs, with the CPU idle, through main's pipe path and through stage 3's
region alike. Round 3 of PR #905, one boot per image, the same 170,439,044-byte
file each run, at a 16 ms round trip:

| image | run 1 | runs 2 to 6 |
|---|---|---|
| main `f0a987bc6` with the instrument (pipes) | 12.747 s, 107.0 Mb/s, busy 0.016 | 778 to 868 Mb/s |
| stage 3 (`toyos::net::stream`'s region) | 2.900 s, 470.1 Mb/s, busy 0.057 | 722 to 878 Mb/s |

The round before, stage 3's image read 101,985,020 bytes in 34.4 s (23.7 Mb/s,
busy 0.007) before the job's window cut its first run, which had no later runs
to compare. The readings are on `ToyOSOrg/evidence` branch `pr-905`,
`round2/metal/` and `round3/metal/`. Nothing on the host or in QEMU has shown it.

Measured (PR #933's instrument boot, evidence
`ToyOSOrg/evidence@8e7a724c625d6a2eb3302becbafbfd6c3e93726a`, `instrument/`):
every run's headers were a Fastly HIT, and run 1 (2.803 s, 486 Mb/s against
808 to 867) ran at line rate for 1.25 s and then paused twice, about 400 ms
each. In the first pause the I219 saw 9 frames on the wire and missed none,
netstack offered a window of at least 4.13 MB with nothing unread, and no hole,
retransmission or SACK followed: the sender stopped. Interrupt delivery, the
window and the client's reads are not the cause. That a HIT node pauses
mid-body on a boot's first fetch and not after is an inference about the CDN,
not measured. Unexplained: Ubuntu on the same T14 ran its first fetch at 827
to 857 Mb/s in six sessions on 2026-10-12.

The row is not on main; its job on the measurement tree
`wt/toyos-first-run-instrument` (f260b64de) fetches the file once, untimed and
instrumented, before its timed runs.

**Exit condition**: on the T14, `internet_download`'s first timed run after the
warm-up falls within the band of the boot's other timed runs.

**Owner**: netstack's track, as the row's home.
