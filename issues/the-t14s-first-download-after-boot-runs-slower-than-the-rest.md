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

Candidates, none measured: the I219 driven in MSI mode since #917, which came
into main between the first boot that downloaded fast (the scout's, all six
runs whole) and these; the client's wake to read; or a connection's warm-up in
netstack ([tcp]'s receive window grows by what is read per round trip, and a
lent ring's growth is shown by `a_lent_receive_ring_grows_the_window_as_a_read_does`).

**Exit condition**: on the T14, `internet_download`'s first run falls within
the band of the boot's other runs.

**Owner**: netstack.
