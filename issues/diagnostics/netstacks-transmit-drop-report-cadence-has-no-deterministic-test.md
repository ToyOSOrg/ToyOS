---
status: open
kind: tooling
opened: 2026-09-25
---

# netd's transmit-drop report cadence has no test that reds on a line per drop

netd says what its NIC counted once a pass, after the pass's `iface.poll`
loop, and never from a driver's `tx`: T14 run 130's `/log` carried the I219's
counter line once per dropped frame, the drop count going 1, 2, 3 ... at one
timestamp, while logd served its backlog to the host. Nothing in the suite
stages a burst of drops, so a driver that reports from `tx` again passes every
test.

Measured in QEMU, `tests/logstreamcase` with a reader served
`test_rs_log_flood`'s five megabytes: QEMU's virtio-net takes transmitted
frames in a bottom half of its main loop, and the guest's queue of 16 fills
only when the host deschedules that loop. Dropped frames, by host load (the
harness's own width factor): 330 at 3.02x with `x-txburst=1`, 40 at 1.34x
with the default device, and 0 twice at 1.30x with `x-txburst=1`. Every
QEMU stall of the transmit path that holds the BQL also blocks the guest's
doorbell, so netd waits instead of dropping. A guest-side hold (the driver
taking heads back once a pass) dropped nothing: logd's data reaches netd's
socket a few segments a pass.

What would stage it on any host: a `-netdev stream` whose host end stops
reading, which makes virtio-net wait on its peer with the guest running, beside
a host-side DHCP responder so netd has an address to send from; or a netd
seam that drives the virtio driver's `tx` over plain memory in a host test.

Exit: a test that is red with the report called from `tx` and green with it
called once a pass, shown on a tree that builds.
