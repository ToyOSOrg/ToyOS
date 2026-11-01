---
status: open
kind: defect
opened: 2026-10-21
---

# `i219_msi` passes only when the network sends it a frame

`i219_messages_on_its_msi_vector` fails unless a frame arrives within
`LISTEN_NANOS` after the job's own DHCP discover
(`tests/guest/rust-tests/src/bin/i219_msi.rs`, "no frame arrived on a message
under traffic"). The frame has to come from a peer on the T14's wire: the
row's receive claim is about whatever traffic the network happens to carry,
and the job controls none of it. Its passing runs counted one or two frames.

On 2026-10-21 the T14's wire was quiet. #974 at `94968aca2` and main
`b580e0078` (with the same test-only output line, `bf581bdeb`) were booted
back to back, and each failed alike. The MAC's own counters read "saw 0
frame(s), took 0 good, missed 0, 0 with CRC errors", while 65 frames were
sent and written back and the link came up after 2.7 s:
https://github.com/ToyOSOrg/evidence/tree/e240b6db1696f7e5c805f1146578d0451e88673a/t14-i219

It failed alike on 2026-10-24 in the T14 `testcases` boot of PR #1062 at
`ff2af0a4d`, whose change touches no NIC, interrupt or network code: every
interrupt reading held, the link came up after 2.8 s, 65 frames were sent and
written back, and the MAC saw 0. #1058's boot at `4ce692580` had passed it
earlier the same day:
https://github.com/ToyOSOrg/evidence/tree/617bbbe20535d92ccbc262a8a0f88a8d7df3e6c3/metal-r2

Exit: the job makes the traffic it receives, by MAC or PHY loopback or a
frame addressed to itself, so its receive claim no longer depends on the
network sending anything. Its verdict is then the same on a quiet wire and a
busy one.
